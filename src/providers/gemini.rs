//! Native Gemini GenerateContent. The exact model parts belong to the current
//! tool loop so each function result can be matched to its signed call.
use super::{Completion, Tool, checked, frames, request};
use crate::state::{Message, Provider, Role, ToolCall, endpoint};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Url, header};
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

fn model_id(name: &str) -> Option<&str> {
    let id = name.strip_prefix("models/").unwrap_or(name);
    (!id.is_empty()
        && id.len() <= 300
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)))
    .then_some(id)
}

fn models_url(provider: &Provider) -> Result<Url> {
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Gemini base URL."))?
        .pop_if_empty()
        .push("models");
    Ok(url)
}

pub(super) async fn discover(client: &Client, provider: &Provider) -> Result<Vec<String>> {
    let mut url = models_url(provider)?;
    let mut models = Vec::new();
    let mut cursors = HashSet::new();
    for _ in 0..20 {
        let response = checked(request(provider, client.get(url.clone()))?.send().await?).await?;
        let body: Value = response
            .json()
            .await
            .context("The Gemini model inventory was not JSON.")?;
        if let Some(page) = body["models"].as_array() {
            models.extend(page.iter().filter_map(|model| {
                let supports_chat = model["supportedGenerationMethods"]
                    .as_array()?
                    .iter()
                    .any(|method| method == "generateContent");
                if supports_chat {
                    model_id(model["name"].as_str()?).map(str::to_owned)
                } else {
                    None
                }
            }));
        } else {
            // An empty list may be omitted by the API's JSON encoder.
            ensure!(
                body.as_object()
                    .is_some_and(|fields| fields.is_empty() || fields.contains_key("nextPageToken"))
                    && body["models"].is_null(),
                "The endpoint returned an incompatible Gemini model inventory."
            );
        }
        let Some(cursor) = body["nextPageToken"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
        else {
            models.sort_unstable();
            models.dedup();
            return Ok(models);
        };
        ensure!(
            cursors.insert(cursor.to_owned()),
            "Gemini returned a repeated model page token."
        );
        url.query_pairs_mut()
            .clear()
            .append_pair("pageToken", cursor);
    }
    bail!("The Gemini model inventory exceeds the page limit. Add model IDs manually.")
}

fn content_parts(message: &Message) -> Vec<Value> {
    let mut parts = Vec::new();
    if message.has_content() {
        parts.push(json!({"text":message.content()}));
    }
    if let Some(file) = &message.attachment
        && let Some(mime) = &file.media_type
    {
        parts.push(json!({"inlineData":{"mimeType":mime,"data":file.text}}));
    }
    parts
}

fn content_history(messages: &[Message]) -> Result<Vec<Value>> {
    let mut contents: Vec<Value> = Vec::new();
    let mut native_ids = HashSet::new();
    let mut results = Vec::new();
    for message in messages {
        if message.role == Role::Tool {
            let name = message
                .tool_name
                .as_deref()
                .context("Gemini tool result is missing its name.")?;
            let mut response = json!({"name":name,"response":{"output":message.text}});
            if let Some(id) = &message.tool_id
                && native_ids.contains(id.as_str())
            {
                response["id"] = json!(id);
            }
            results.push(json!({"functionResponse":response}));
            continue;
        }
        // One user content contains all responses to the preceding parallel
        // call batch in order. Keep it distinct from new user text.
        if !results.is_empty() {
            contents.push(json!({"role":"user","parts":std::mem::take(&mut results)}));
        }
        let (role, parts) = match message.role {
            Role::User => ("user", content_parts(message)),
            Role::Assistant if !message.output_items.is_empty() => {
                native_ids.extend(
                    message
                        .output_items
                        .iter()
                        .filter_map(|part| part["functionCall"]["id"].as_str()),
                );
                ("model", message.output_items.clone())
            }
            Role::Assistant => ("model", content_parts(message)),
            Role::Tool => unreachable!("tool results are grouped above"),
        };
        if !parts.is_empty() {
            contents.push(json!({"role":role,"parts":parts}));
        }
    }
    if !results.is_empty() {
        contents.push(json!({"role":"user","parts":results}));
    }
    Ok(contents)
}

fn payload(
    provider: &Provider,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
) -> Result<Value> {
    let mut body = json!({"contents":content_history(messages)?});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts":[{"text":system}]});
    }
    if let Some(limit) = provider.token_limit {
        body["generationConfig"] = json!({"maxOutputTokens":limit});
    }
    if !tools.is_empty() {
        let declarations: Vec<_> = tools.iter().map(|tool| json!({
            "name":tool.name, "description":tool.description, "parametersJsonSchema":tool.schema
        })).collect();
        body["tools"] = json!([{"functionDeclarations":declarations}]);
    }
    Ok(body)
}

#[derive(Default)]
struct Stream {
    answer: Completion,
    bytes: usize,
    done: bool,
}

impl Stream {
    fn decode(&mut self, event: &Value, delta: &mut impl FnMut(&str)) -> Result<()> {
        if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
            bail!(
                "{}",
                error["message"]
                    .as_str()
                    .unwrap_or("Gemini reported a stream error.")
            );
        }
        if let Some(reason) = event["promptFeedback"]["blockReason"].as_str()
            && reason != "BLOCK_REASON_UNSPECIFIED"
        {
            bail!("Gemini blocked the prompt ({reason}).");
        }
        let candidate = &event["candidates"][0];
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                self.bytes += part.to_string().len();
                ensure!(
                    self.bytes <= 8 * 1024 * 1024,
                    "The Gemini response exceeds the size limit."
                );
                if let Some(text) = part["text"].as_str()
                    && part["thought"] != true
                {
                    self.answer.text.push_str(text);
                    delta(text);
                }
                if let Some(call) = part.get("functionCall") {
                    ensure!(
                        call["partialArgs"].as_array().is_none_or(Vec::is_empty)
                            && call["willContinue"] != true,
                        "The endpoint returned a partial Gemini function call."
                    );
                    ensure!(
                        self.answer.calls.len() < 64,
                        "Gemini returned too many function calls."
                    );
                    let name = call["name"]
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .context("Gemini returned a function call without a name.")?;
                    let id = call["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("call_{}", Uuid::new_v4()));
                    ensure!(
                        !self.answer.calls.iter().any(|call| call.id == id),
                        "Gemini repeated a function call ID."
                    );
                    let arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    ensure!(
                        arguments.is_object(),
                        "Gemini returned invalid function arguments."
                    );
                    self.answer.calls.push(ToolCall {
                        id,
                        name: name.into(),
                        arguments,
                    });
                }
                // Never merge a signed part with another part in a tool loop.
                self.answer.output_items.push(part.clone());
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            match reason {
                "STOP" => self.done = true,
                "" | "FINISH_REASON_UNSPECIFIED" => {}
                "MAX_TOKENS" => bail!("Gemini reached its output token limit."),
                _ => bail!("Gemini ended generation ({reason})."),
            }
        }
        Ok(())
    }

    fn complete(self) -> Result<Completion> {
        ensure!(
            self.done,
            "Gemini closed the stream before reporting completion."
        );
        ensure!(
            !self.answer.text.is_empty() || !self.answer.calls.is_empty(),
            "Gemini returned no text or function calls. Choose a text-capable model."
        );
        Ok(self.answer)
    }
}

pub(super) async fn generate(
    client: &Client,
    provider: &Provider,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
    mut delta: impl FnMut(&str),
) -> Result<Completion> {
    let model =
        model_id(model).context("Enter a Gemini model ID, optionally prefixed with models/.")?;
    let mut url = models_url(provider)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Gemini base URL."))?
        .push(&format!("{model}:streamGenerateContent"));
    url.query_pairs_mut().append_pair("alt", "sse");
    let response = checked(
        request(provider, client.post(url))?
            .json(&payload(provider, system, messages, tools)?)
            .send()
            .await?,
    )
    .await?;
    let mut stream = Stream::default();
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
    {
        // Some native-format gateways return the standard response or an
        // array of chunks even when the client requests SSE.
        let body: Value = response.json().await?;
        for event in body
            .as_array()
            .map_or_else(|| std::slice::from_ref(&body), Vec::as_slice)
        {
            stream.decode(event, &mut delta)?;
            if stream.done {
                break;
            }
        }
    } else {
        frames(response, true, |frame| {
            let event = serde_json::from_str(frame).context("Invalid Gemini stream event.")?;
            stream.decode(&event, &mut delta)?;
            Ok(stream.done)
        })
        .await?;
    }
    stream.complete()
}
