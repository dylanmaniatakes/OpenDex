//! Provider adapters share a tool and transcript representation. Each adapter owns its
//! request/stream formats so its native API remains usable through gateways.
mod gemini;
pub mod remote;
mod wire;

use crate::state::AnthropicAuth;
use crate::state::{Message, Provider, ProviderKind, ToolCall, endpoint, read_secret};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, Response, header};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Debug, Default)]
pub struct Completion {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub output_items: Vec<Value>,
}

pub(crate) enum HermesControl {
    Started(String),
    Approval(Value),
}

pub(crate) struct HermesEvents<'a> {
    pub controls: mpsc::Sender<HermesControl>,
    pub activity: &'a (dyn Fn(HermesActivity<'_>) + Sync),
}

pub(crate) struct HermesActivity<'a> {
    pub notice: Option<&'a str>,
    pub tool_running: bool,
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(120))
        .user_agent(concat!("OpenDex/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub(crate) async fn request(
    _client: &Client,
    provider: &Provider,
    request: RequestBuilder,
) -> Result<RequestBuilder> {
    ensure!(
        provider.kind != ProviderKind::Anthropic
            || provider.anthropic_auth != AnthropicAuth::Account,
        "Claude account connections use the installed Claude Code executable."
    );
    let secret = read_secret(
        provider.credential_id(),
        &provider.endpoint,
        provider.has_secret,
    )?;
    let request = request_with_secret(provider, request, secret.as_deref())?;
    Ok(request)
}

fn request_with_secret(
    provider: &Provider,
    mut request: RequestBuilder,
    secret: Option<&str>,
) -> Result<RequestBuilder> {
    if (matches!(
        provider.kind,
        ProviderKind::OpenAi | ProviderKind::Gemini | ProviderKind::OllamaCloud
    ) || (provider.kind == ProviderKind::Anthropic
        && provider.anthropic_auth == AnthropicAuth::Api))
        && secret.is_none()
        && provider.headers.is_empty()
    {
        bail!("Save an API key for {} in Settings.", provider.name);
    }
    if provider.kind == ProviderKind::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if let Some(secret) = secret {
        let name = provider.key_header().unwrap_or("authorization");
        ensure!(
            !provider
                .headers
                .iter()
                .any(|(header, _)| header.eq_ignore_ascii_case(name)),
            "The API key header {name} is also configured through an environment variable."
        );
        request = if let Some(header) = provider.key_header() {
            let mut value = header::HeaderValue::from_str(secret).context("Invalid key format.")?;
            value.set_sensitive(true);
            request.header(header, value)
        } else {
            request.bearer_auth(secret)
        };
    }
    for (header, variable) in &provider.headers {
        let value = std::env::var(variable).map_err(|_| {
            anyhow::anyhow!("Environment variable {variable} is unavailable to OpenDex.")
        })?;
        let mut value = header::HeaderValue::from_str(&value).map_err(|_| {
            anyhow::anyhow!("Environment variable {variable} contains an invalid header value.")
        })?;
        value.set_sensitive(true);
        request = request.header(header, value);
    }
    Ok(request)
}

pub async fn discover(client: &Client, provider: &Provider) -> Result<Vec<String>> {
    if provider.kind == ProviderKind::Gemini {
        return gemini::discover(client, provider).await;
    }
    let mut base = endpoint(&provider.endpoint)?;
    let suffix = match provider.kind {
        ProviderKind::Ollama | ProviderKind::OllamaCloud => "tags",
        ProviderKind::ChatGptAccount => bail!("Use the ChatGPT account connection."),
        _ => "models",
    };
    base.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid API endpoint."))?
        .pop_if_empty()
        .push(suffix);

    let mut models = Vec::new();
    for _ in 0..20 {
        // The cloud catalog is public and deliberately uses the fixed, official endpoint.
        let get = if provider.kind == ProviderKind::OllamaCloud {
            client.get("https://ollama.com/api/tags")
        } else {
            request(client, provider, client.get(base.clone())).await?
        };
        let response = checked(get.send().await?).await?;
        let body: Value = response
            .json()
            .await
            .context("The model inventory was not JSON.")?;
        let (array, key) = if suffix == "tags" {
            ("models", "name")
        } else {
            ("data", "id")
        };
        let entries = body[array]
            .as_array()
            .context("The endpoint returned an incompatible model inventory.")?;
        models.extend(
            entries
                .iter()
                .filter_map(|model| model[key].as_str().map(str::to_owned)),
        );
        if provider.kind != ProviderKind::Anthropic || body["has_more"] != true {
            break;
        }
        let after = body["last_id"]
            .as_str()
            .context("Missing model inventory page cursor.")?;
        base.query_pairs_mut()
            .clear()
            .append_pair("after_id", after);
    }
    models.sort_unstable();
    models.dedup();
    Ok(models)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecution {
    Local,
    Hermes,
}

/// A saved approval preference does not select the execution host. The server
/// capability check distinguishes Hermes Agent from compatible inference.
pub async fn tool_execution(
    client: &Client,
    provider: &Provider,
    model: &str,
) -> Result<ToolExecution> {
    if provider.kind != ProviderKind::OpenAiCompatible {
        return Ok(ToolExecution::Local);
    }
    let hermes_alias = Provider::is_hermes_agent_model(model);
    let message = "This is the Hermes Agent API. Enable Hermes VPS approvals for this connection, or choose a local execution model in Provider settings.";
    ensure!(
        provider.hermes_remote_approvals || !hermes_alias,
        "{message}"
    );
    ensure!(
        !Provider::is_openclaw_agent_model(model),
        "This is an OpenClaw agent target. Choose a local execution model for this connection in Provider settings."
    );
    let timeout = if provider.hermes_remote_approvals {
        5
    } else {
        2
    };
    let description = endpoint_capabilities(client, provider, timeout).await?;
    if let Some(description) = &description {
        let hermes = description["object"] == "hermes.api_server.capabilities"
            || description["platform"] == "hermes-agent";
        if hermes {
            ensure!(provider.hermes_remote_approvals, "{message}");
            ensure!(
                remote::supports_approvals(description),
                "This endpoint does not advertise Hermes command approvals. Update the Hermes Agent server or choose a local execution model."
            );
            return Ok(ToolExecution::Hermes);
        }
        ensure!(
            description["runtime"]["tool_execution"] != "server"
                || description["runtime"]["split_runtime"] == true,
            "This endpoint runs agent tools on its server. Choose a local execution model for this connection in Provider settings."
        );
    }
    ensure!(
        !hermes_alias,
        "Could not verify Hermes command approval support. Check the Agent API URL and key in Provider settings."
    );
    if provider.hermes_remote_approvals && description.is_none() {
        // The Hermes subscription proxy serves /models but does not expose
        // /capabilities. A Hermes Agent catalog can use a custom model alias.
        let catalog = endpoint_metadata(client, provider, "models", timeout, 512 * 1024).await?;
        ensure!(
            catalog
                .as_ref()
                .and_then(|catalog| catalog["data"].as_array())
                .is_some_and(|models| {
                    models.iter().any(|entry| entry["id"] == model)
                        && !models.iter().any(|entry| entry["owned_by"] == "hermes")
                }),
            "Could not confirm whether this connection runs tools on the server. Check the Agent API URL and key, or turn off Hermes VPS approvals for an inference-only connection."
        );
    }
    Ok(ToolExecution::Local)
}

async fn endpoint_capabilities(
    client: &Client,
    provider: &Provider,
    timeout: u64,
) -> Result<Option<Value>> {
    endpoint_metadata(client, provider, "capabilities", timeout, 32 * 1024).await
}

async fn endpoint_metadata(
    client: &Client,
    provider: &Provider,
    path: &str,
    timeout: u64,
    limit: usize,
) -> Result<Option<Value>> {
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid API endpoint."))?
        .pop_if_empty()
        .push(path);
    let probe = request(client, provider, client.get(url)).await?;
    if let Ok(response) = probe.timeout(Duration::from_secs(timeout)).send().await
        && response.status().is_success()
        && let Ok(body) = bounded_text(response, limit).await
    {
        return Ok(serde_json::from_str::<Value>(&body)
            .ok()
            .filter(Value::is_object));
    }
    Ok(None)
}

pub(crate) async fn hermes_remote_completion(
    client: &Client,
    provider: &Provider,
    model: &str,
    system: &str,
    messages: &[Message],
    events: HermesEvents<'_>,
    delta: impl FnMut(&str),
) -> Result<Completion> {
    let response = generation_response(client, provider, model, system, messages, &[]).await?;
    ensure!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "Hermes did not open a stream that can carry approvals."
    );
    decode_generation(response, provider.kind, delta, Some(&events)).await
}

pub(crate) async fn resolve_hermes_approval(
    client: &Client,
    provider: &Provider,
    approval: &Value,
    allow: bool,
) -> Result<()> {
    let run = approval["run_id"]
        .as_str()
        .context("Hermes did not include a run ID for this approval.")?;
    let request_id = approval["request_id"]
        .as_str()
        .or_else(|| approval["prompt_id"].as_str())
        .filter(|id| !id.trim().is_empty() && id.len() <= 256)
        .context("Hermes did not include an exact approval request ID.")?;
    let mut body = serde_json::json!({
        "request_id": request_id,
        "choice": if allow { "once" } else { "deny" }
    });
    if let Some(generation) = approval["execution_generation"].as_u64() {
        body["execution_generation"] = serde_json::json!(generation);
    }
    let url = hermes_control_endpoint(provider, run, "approval")?;
    let request = request(client, provider, client.post(url)).await?;
    checked(
        request
            .timeout(Duration::from_secs(10))
            .json(&body)
            .send()
            .await?,
    )
    .await
    .context("Hermes did not accept the approval decision.")?;
    Ok(())
}

pub(crate) async fn stop_hermes_run(client: &Client, provider: &Provider, run: &str) -> Result<()> {
    let url = hermes_control_endpoint(provider, run, "stop")?;
    let request = request(client, provider, client.post(url)).await?;
    checked(request.timeout(Duration::from_secs(10)).send().await?).await?;
    Ok(())
}

fn hermes_control_endpoint(provider: &Provider, run: &str, action: &str) -> Result<reqwest::Url> {
    ensure!(
        !run.is_empty()
            && run.len() <= 128
            && run
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
        "Hermes did not include a valid run ID."
    );
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Hermes API endpoint."))?
        .pop_if_empty()
        .extend(["runs", run, action]);
    Ok(url)
}

pub async fn generate(
    client: &Client,
    provider: &Provider,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
    delta: impl FnMut(&str),
) -> Result<Completion> {
    if provider.kind == ProviderKind::Gemini {
        return gemini::generate(client, provider, model, system, messages, tools, delta).await;
    }
    let response = generation_response(client, provider, model, system, messages, tools).await?;
    decode_generation(response, provider.kind, delta, None).await
}

async fn generation_response(
    client: &Client,
    provider: &Provider,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
) -> Result<Response> {
    let (path, payload) = wire::payload(provider, model, system, messages, tools)?;
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid API endpoint."))?
        .pop_if_empty()
        .extend(path.split('/'));
    checked(
        request(client, provider, client.post(url))
            .await?
            .json(&payload)
            .send()
            .await?,
    )
    .await
}

async fn decode_generation(
    response: Response,
    kind: ProviderKind,
    mut delta: impl FnMut(&str),
    hermes: Option<&HermesEvents<'_>>,
) -> Result<Completion> {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let sse = content_type.starts_with("text/event-stream");
    if content_type.starts_with("application/json")
        && !matches!(kind, ProviderKind::Ollama | ProviderKind::OllamaCloud)
    {
        let body = response.json().await?;
        let outcome = wire::whole(kind, &body)?;
        delta(&outcome.text);
        return Ok(outcome);
    }

    let mut parser = wire::Decoder::new(kind);
    let mut run_id = None;
    let mut previous_status = String::new();
    frames_with_event(response, sse, |event_name, frame| {
        if frame == "[DONE]" {
            parser.finish();
        } else {
            if hermes.is_some() && matches!(event_name, Some("ping" | "keepalive")) {
                return Ok(false);
            }
            let value: Value = serde_json::from_str(frame)
                .context("The provider returned a malformed stream event.")?;
            if let Some(hermes) = hermes {
                let name = event_name
                    .filter(|name| *name != "message")
                    .or_else(|| value["event"].as_str());
                let choice = &value["choices"][0];
                let mut note = |status: &str, tool_running| {
                    let notice = if previous_status == status {
                        None
                    } else {
                        previous_status.clear();
                        previous_status.push_str(status);
                        Some(status)
                    };
                    (hermes.activity)(HermesActivity {
                        notice,
                        tool_running,
                    });
                };
                match choice["finish_reason"].as_str() {
                    Some("cancelled") => bail!("The Hermes server cancelled this turn."),
                    Some("error") => {
                        if let Some(message) = value["error"]["message"]
                            .as_str()
                            .filter(|message| !message.trim().is_empty())
                        {
                            bail!(
                                "Hermes reported a failed turn. {}",
                                message.chars().take(320).collect::<String>()
                            );
                        }
                        bail!("The Hermes server reported a failed turn.");
                    }
                    _ => {}
                }
                if run_id.is_none()
                    && let Some(id) = value["run_id"]
                        .as_str()
                        .or_else(|| (!choice.is_null()).then(|| value["id"].as_str()).flatten())
                {
                    hermes
                        .controls
                        .try_send(HermesControl::Started(id.to_owned()))
                        .context("Could not receive the Hermes run ID.")?;
                    run_id = Some(id.to_owned());
                    note("Hermes connected. Waiting for its model…", false);
                }
                match name {
                    Some("approval.request") => {
                        ensure!(
                            value["run_id"].as_str().is_some()
                                && value["run_id"].as_str() == run_id.as_deref(),
                            "Hermes reported an approval without the current run ID."
                        );
                        (hermes.activity)(HermesActivity {
                            notice: None,
                            tool_running: true,
                        });
                        hermes
                            .controls
                            .try_send(HermesControl::Approval(value))
                            .context("Too many pending Hermes approvals.")?;
                        return Ok(false);
                    }
                    Some("clarify.request") => bail!(
                        "Hermes requested clarification. Add the missing detail before retrying."
                    ),
                    Some("hermes.status") => {
                        if let Some(status) = value["text"]
                            .as_str()
                            .filter(|status| !status.trim().is_empty())
                        {
                            note(
                                &format!(
                                    "Hermes · {}",
                                    status.chars().take(280).collect::<String>()
                                ),
                                false,
                            );
                        }
                        return Ok(false);
                    }
                    Some("hermes.tool.progress") => {
                        let tool: String = value["tool"]
                            .as_str()
                            .unwrap_or("tool")
                            .chars()
                            .take(64)
                            .collect();
                        let running = value["status"] != "completed";
                        let status = if running { "running" } else { "finished" };
                        note(&format!("Hermes · {status} {tool}"), running);
                        return Ok(false);
                    }
                    _ => {}
                }
                if choice["delta"]["reasoning_content"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
                {
                    note("Hermes is reasoning…", false);
                }
                if choice["delta"]["content"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
                    || choice["delta"]["refusal"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty())
                {
                    note("Hermes is replying…", false);
                }
            }
            parser.event(&value, &mut delta)?;
        }
        Ok(parser.finished())
    })
    .await?;
    parser.complete()
}

/// SSE frame boundaries can cross chunks and contain multiple data lines.
/// NDJSON uses the same byte-oriented buffering before UTF-8 decoding.
pub(crate) async fn frames(
    response: Response,
    sse: bool,
    mut on_frame: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    frames_with_event(response, sse, |_, frame| on_frame(frame)).await
}

async fn frames_with_event(
    response: Response,
    sse: bool,
    mut on_frame: impl FnMut(Option<&str>, &str) -> Result<bool>,
) -> Result<()> {
    const LIMIT: usize = 8 * 1024 * 1024;
    let mut stream = response.bytes_stream();
    let mut line = Vec::new();
    let mut data = String::new();
    let mut event = None::<String>;

    let mut process_line = |line: &[u8]| -> Result<bool> {
        let line = std::str::from_utf8(line.strip_suffix(b"\r").unwrap_or(line))?;
        if !sse {
            return if line.trim().is_empty() {
                Ok(false)
            } else {
                on_frame(None, line)
            };
        }
        if line.is_empty() {
            let name = event.take();
            if data.is_empty() {
                return Ok(false);
            }
            data.pop();
            let done = on_frame(name.as_deref(), &data)?;
            data.clear();
            return Ok(done);
        }
        if let Some(name) = line.strip_prefix("event:") {
            event = Some(name.strip_prefix(' ').unwrap_or(name).to_owned());
        }
        if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            ensure!(
                data.len() + value.len() < LIMIT,
                "Stream event exceeds the size limit."
            );
            data.push_str(value);
            data.push('\n');
        }
        Ok(false)
    };

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
            let terminated = piece.ends_with(b"\n");
            let piece = if terminated {
                &piece[..piece.len() - 1]
            } else {
                piece
            };
            ensure!(
                line.len() + piece.len() <= LIMIT,
                "Stream line exceeds the size limit."
            );
            line.extend_from_slice(piece);
            if terminated {
                if process_line(&line)? {
                    return Ok(());
                }
                line.clear();
            }
        }
    }
    if !line.is_empty() && process_line(&line)? {
        return Ok(());
    }
    if sse {
        process_line(b"")?;
    }
    Ok(())
}

pub(crate) async fn checked(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let mut stream = response.bytes_stream();
    let mut data = Vec::new();
    while let Some(Ok(chunk)) = stream.next().await {
        let length = chunk.len().min(4096 - data.len());
        data.extend_from_slice(&chunk[..length]);
        if data.len() >= 4096 {
            break;
        }
    }
    let raw = String::from_utf8_lossy(&data);
    let parsed = serde_json::from_slice::<Value>(&data).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| {
            value["error"]["message"]
                .as_str()
                .or(value["error"].as_str())
                .or(value["message"].as_str())
        })
        .unwrap_or(if raw.trim_start().starts_with('<') {
            ""
        } else {
            raw.trim()
        });
    let message: String = message.chars().take(600).collect();
    if message.is_empty() {
        bail!("Endpoint returned HTTP {status}.");
    }
    bail!("Endpoint returned HTTP {status}: {message}");
}

/// Parse the official Ollama search page's server-rendered library links.
/// Keep this conservative: only `library/{model}` result cards are accepted.
pub fn parse_ollama_search(html: &str) -> Vec<String> {
    const PREFIX: &str = "href=\"/library/";
    let mut models = Vec::new();
    let mut cursor = 0;
    while cursor < html.len() {
        let Some(start) = html[cursor..].find(PREFIX) else {
            break;
        };
        let start = cursor + start + PREFIX.len();
        let Some(length) = html[start..].find('"') else {
            break;
        };
        let id = &html[start..start + length];
        if validate_model_id(id) {
            models.push(id.to_owned());
        }
        cursor = start + length;
    }
    models.sort_unstable();
    models.dedup();
    models
}

fn validate_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 300
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_'
        })
}

pub async fn search_ollama(client: &Client, query: &str) -> Result<Vec<String>> {
    let query = query.trim();
    ensure!(!query.is_empty(), "Enter a search term for Ollama.");
    ensure!(
        query.len() <= 200,
        "Keep the Ollama search term under 200 characters."
    );
    let mut url = reqwest::Url::parse("https://ollama.com/search")
        .context("Ollama search URL is unavailable.")?;
    url.query_pairs_mut().append_pair("q", query);
    let response = checked(client.get(url).send().await?).await?;
    let body = bounded_text(response, MAX_REMOTE_CATALOG_BYTES)
        .await
        .context("Ollama search returned unreadable content.")?;
    let models = parse_ollama_search(&body);
    ensure!(
        !models.is_empty(),
        "Ollama returned no matching models. Try a shorter or different search term."
    );
    Ok(models)
}

const MAX_REMOTE_CATALOG_BYTES: usize = 4 * 1024 * 1024;

pub(crate) async fn bounded_text(mut response: Response, limit: usize) -> Result<String> {
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "The remote catalog response exceeds the size limit."
    );
    let mut body = Vec::with_capacity(limit.min(256 * 1024));
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= limit,
            "The remote catalog response exceeds the size limit."
        );
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hermes_named_status_and_approval_events_survive_heartbeats() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            sync::Mutex,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = BufReader::new(&socket).lines();
            while !request.next().unwrap().unwrap().is_empty() {}
            let body = concat!(
                "data: {\"id\":\"run-fixture\",\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                ": keepalive\n\n",
                "event: keepalive\ndata: heartbeat\n\n",
                "event: hermes.status\ndata: {\"text\":\"Waiting for the upstream provider\"}\n\n",
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"fixture thinking\"}}]}\n\n",
                "event: hermes.tool.progress\ndata: {\"tool\":\"terminal\",\"status\":\"running\"}\n\n",
                "event: approval.request\ndata: {\"run_id\":\"run-fixture\",\"request_id\":\"prompt-fixture\"}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"},\"finish_reason\":\"stop\"}]}\n\n",
            );
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let response = client().unwrap().get(url).send().await.unwrap();
        let (send, mut receive) = mpsc::channel(8);
        let recorded = Mutex::new(Vec::new());
        let activity = |update: HermesActivity<'_>| {
            recorded
                .lock()
                .unwrap()
                .push((update.notice.map(str::to_owned), update.tool_running));
        };
        let events = HermesEvents {
            controls: send,
            activity: &activity,
        };
        let answer = decode_generation(
            response,
            ProviderKind::OpenAiCompatible,
            |_| {},
            Some(&events),
        )
        .await
        .unwrap();
        assert_eq!(answer.text, "Done.");
        assert!(
            matches!(receive.try_recv().unwrap(), HermesControl::Started(id) if id == "run-fixture")
        );
        assert!(
            matches!(receive.try_recv().unwrap(), HermesControl::Approval(value) if value["request_id"] == "prompt-fixture")
        );
        let notices = recorded.into_inner().unwrap();
        assert_eq!(notices.len(), 6);
        assert_eq!(
            notices[1].0.as_deref(),
            Some("Hermes · Waiting for the upstream provider")
        );
        assert_eq!(notices[2].0.as_deref(), Some("Hermes is reasoning…"));
        assert_eq!(notices[3], (Some("Hermes · running terminal".into()), true));
        server.join().unwrap();
    }

    #[test]
    fn saved_keys_use_sensitive_adapter_or_custom_headers() {
        let client = client().unwrap();
        let url = "http://127.0.0.1:3000";
        let mut provider = Provider {
            kind: ProviderKind::Gemini,
            ..Provider::local()
        };

        let default = request_with_secret(&provider, client.get(url), Some("example-key"))
            .unwrap()
            .build()
            .unwrap();
        let google_key = default.headers().get("x-goog-api-key").unwrap();
        assert_eq!(google_key, "example-key");
        assert!(google_key.is_sensitive());
        assert!(!default.headers().contains_key("authorization"));

        provider.api_key_header = Some(" X-Gateway-Key ".into());
        provider.validate().unwrap();
        let custom = request_with_secret(&provider, client.get(url), Some("example-key"))
            .unwrap()
            .build()
            .unwrap();
        let gateway_key = custom.headers().get("x-gateway-key").unwrap();
        assert_eq!(gateway_key, "example-key");
        assert!(gateway_key.is_sensitive());
        assert!(!custom.headers().contains_key("x-goog-api-key"));

        provider.headers = vec![("X-Gateway-Key".into(), "PATH".into())];
        assert!(request_with_secret(&provider, client.get(url), Some("example-key")).is_err());

        provider.api_key_header = Some("Host".into());
        assert!(provider.validate().is_err());
    }

    #[tokio::test]
    async fn anthropic_account_requests_never_use_direct_http_authentication() {
        let client = client().unwrap();
        let mut provider = Provider::local();
        provider.kind = ProviderKind::Anthropic;
        provider.anthropic_auth = AnthropicAuth::Account;
        let error = request(&client, &provider, client.get("http://127.0.0.1:3000"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("installed Claude Code"));
    }
}
