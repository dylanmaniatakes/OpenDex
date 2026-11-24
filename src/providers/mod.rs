//! Provider adapters share a tool and transcript representation. Each adapter owns its
//! request/stream formats so its native API remains usable through gateways.
mod gemini;
mod wire;

use crate::state::{Message, Provider, ProviderKind, ToolCall, endpoint, read_secret};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, Response, header};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

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

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(120))
        .user_agent(concat!("OpenDex/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub(crate) fn request(provider: &Provider, request: RequestBuilder) -> Result<RequestBuilder> {
    let secret = read_secret(
        provider.credential_id(),
        &provider.endpoint,
        provider.has_secret,
    )?;
    request_with_secret(provider, request, secret.as_deref())
}

fn request_with_secret(
    provider: &Provider,
    mut request: RequestBuilder,
    secret: Option<&str>,
) -> Result<RequestBuilder> {
    if matches!(
        provider.kind,
        ProviderKind::OpenAi
            | ProviderKind::Anthropic
            | ProviderKind::Gemini
            | ProviderKind::OllamaCloud
    ) && secret.is_none()
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
            request(provider, client.get(base.clone()))?
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

pub async fn generate(
    client: &Client,
    provider: &Provider,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
    mut delta: impl FnMut(&str),
) -> Result<Completion> {
    if provider.kind == ProviderKind::Gemini {
        return gemini::generate(client, provider, model, system, messages, tools, delta).await;
    }
    let (path, payload) = wire::payload(provider, model, system, messages, tools)?;
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid API endpoint."))?
        .pop_if_empty()
        .extend(path.split('/'));
    let response = checked(
        request(provider, client.post(url))?
            .json(&payload)
            .send()
            .await?,
    )
    .await?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let sse = content_type.starts_with("text/event-stream");
    if content_type.starts_with("application/json")
        && !matches!(
            provider.kind,
            ProviderKind::Ollama | ProviderKind::OllamaCloud
        )
    {
        let body = response.json().await?;
        let outcome = wire::whole(provider.kind, &body)?;
        delta(&outcome.text);
        return Ok(outcome);
    }

    let mut parser = wire::Decoder::new(provider.kind);
    frames(response, sse, |frame| {
        if frame == "[DONE]" {
            parser.finish();
        } else {
            let value = serde_json::from_str(frame)
                .context("The provider returned a malformed stream event.")?;
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
    const LIMIT: usize = 8 * 1024 * 1024;
    let mut stream = response.bytes_stream();
    let mut line = Vec::new();
    let mut data = String::new();

    let mut process_line = |line: &[u8]| -> Result<bool> {
        let line = std::str::from_utf8(line.strip_suffix(b"\r").unwrap_or(line))?;
        if !sse {
            return if line.trim().is_empty() {
                Ok(false)
            } else {
                on_frame(line)
            };
        }
        if line.is_empty() && !data.is_empty() {
            data.pop();
            let done = on_frame(&data)?;
            data.clear();
            return Ok(done);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
