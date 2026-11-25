//! Approval-aware delegation to a server agent over the Hermes Runs protocol.
use super::{
    bounded_text, checked, endpoint_capabilities, frames, hermes_control_endpoint, request,
    resolve_hermes_approval, stop_hermes_run,
};
use crate::state::{Provider, ProviderKind, endpoint};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, header};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) fn supports_approvals(description: &Value) -> bool {
    let features = &description["features"];
    (description["object"] == "hermes.api_server.capabilities"
        || description["platform"] == "hermes-agent")
        && features["approval_events"] == true
        && (features["run_approval_response"] == true || features["run_approval"] == true)
}

pub async fn delegate<F: Future<Output = bool>>(
    client: &Client,
    provider: &Provider,
    model: &str,
    task: &str,
    cancel: &CancellationToken,
    mut review: impl FnMut(Value) -> F,
) -> Result<Option<String>> {
    ensure!(
        provider.kind == ProviderKind::OpenAiCompatible && provider.hermes_remote_approvals,
        "Remote delegation is off for this connection."
    );
    let Some(capabilities) = cancel
        .run_until_cancelled(endpoint_capabilities(client, provider, 5))
        .await
    else {
        return Ok(None);
    };
    let capabilities = capabilities?;
    let supported = capabilities.as_ref().is_some_and(|description| {
        supports_approvals(description)
            && ["run_submission", "run_events_sse", "run_stop"]
                .iter()
                .all(|feature| description["features"][*feature] == true)
    });
    ensure!(
        supported,
        "This endpoint does not advertise the approval-aware Hermes Runs API."
    );
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .expect("HTTP base URL")
        .pop_if_empty()
        .push("runs");
    // Once submitted, wait briefly for the run ID even if the user clicks Stop.
    // That ID allows us to interrupt the right run without starting another one.
    let response = checked(
        request(client, provider, client.post(url))
            .await?
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&json!({
                "input": task, "model": model,
                "instructions": "OpenDex delegated this VPS task. Work only on the named remote task. Use your server's approval flow when it asks for a human decision. Do not infer access to the OpenDex computer. Report what ran on the server and what remains pending."
            }))
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .context("Could not confirm VPS task submission. Check the server before retrying.")?,
    )
    .await?;
    let body: Value = serde_json::from_str(&bounded_text(response, 32 * 1024).await?)?;
    let id = body["run_id"]
        .as_str()
        .context("The server did not return a VPS run ID. Check the server before retrying.")?;
    let (pending, mut approvals) = mpsc::channel::<Value>(8);
    let approvals = async {
        while let Some(approval) = approvals.recv().await {
            let allow = review(approval.clone()).await;
            resolve_hermes_approval(client, provider, &approval, allow).await?;
        }
        // The events future owns the sender and reports the terminal outcome.
        std::future::pending().await
    };
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(Outcome::StopRequested),
        _ = tokio::time::sleep(Duration::from_secs(900)) => Err(anyhow::anyhow!("The VPS task exceeded its time limit.")),
        result = run_events(client, provider, id, pending) => result,
        error = approvals => error,
    };
    match outcome {
        Ok(Outcome::Complete(output)) => Ok(Some(output)),
        Ok(Outcome::Failed) => bail!("The VPS agent ended the task without completing it."),
        Ok(Outcome::StopRequested) => {
            stop_hermes_run(client, provider, id)
                .await
                .context("OpenDex could not confirm the VPS stop request.")?;
            Ok(None)
        }
        Err(error) => {
            if let Err(stop) = stop_hermes_run(client, provider, id).await {
                bail!("{error:#} OpenDex could not confirm the VPS stop request: {stop:#}");
            }
            Err(error)
        }
    }
}

enum Outcome {
    Complete(String),
    Failed,
    StopRequested,
}

async fn run_events(
    client: &Client,
    provider: &Provider,
    id: &str,
    pending: mpsc::Sender<Value>,
) -> Result<Outcome> {
    let url = hermes_control_endpoint(provider, id, "events")?;
    let response = checked(
        request(client, provider, client.get(url))
            .await?
            .send()
            .await?,
    )
    .await?;
    ensure!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|mime| mime.starts_with("text/event-stream")),
        "The VPS run did not open an approval event stream."
    );
    let mut outcome = None;
    frames(response, true, |frame| {
        let event: Value = serde_json::from_str(frame).context("Malformed VPS run event.")?;
        ensure!(
            event["run_id"].as_str() == Some(id),
            "The VPS sent an event for a different run."
        );
        match event["event"].as_str() {
            Some("approval.request") => pending
                .try_send(event)
                .context("Too many pending VPS approvals.")?,
            Some("clarify.request") => {
                bail!("The VPS task needs clarification. Refine the task before retrying.")
            }
            Some("run.completed") => {
                let output = event["output"]
                    .as_str()
                    .context("The VPS run returned no result.")?;
                ensure!(
                    output.len() <= 256 * 1024,
                    "The VPS run result is too large."
                );
                outcome = Some(Outcome::Complete(output.to_owned()));
            }
            Some("run.failed" | "run.cancelled" | "run.interrupted" | "run.unknown") => {
                outcome = Some(Outcome::Failed)
            }
            _ => {}
        }
        Ok(outcome.is_some())
    })
    .await?;
    outcome.context("The VPS approval stream disconnected before the task completed.")
}
