use super::{
    Config, MAX_IMAGE_BYTES, MAX_JSON_BYTES, attachment, decode, limited_body, raster_mime,
};
use crate::{
    providers,
    state::{Attachment, endpoint, read_secret, validate_env, validate_request_header},
};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, RequestBuilder, Response, Url, header, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::HashSet, time::Duration};
use uuid::Uuid;

const PROMPT: &str = "{{prompt}}";
pub(super) const MAX_OPTIONS: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    OpenAi,
    Gemini,
    Imagen,
    Stability,
    Ideogram,
    BlackForestLabs,
    Fal,
    Replicate,
    ComfyUi,
    Firefly,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: Uuid,
    pub name: String,
    pub kind: SourceKind,
    pub endpoint: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub options: Map<String, Value>,
    #[serde(default)]
    pub output_pointer: String,
    #[serde(default)]
    pub key_header: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub asset_hosts: Vec<String>,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<Uuid>,
}

impl Source {
    pub fn validate(&mut self) -> Result<()> {
        self.name = self.name.trim().to_owned();
        ensure!(
            !self.name.is_empty()
                && self.name.len() <= 100
                && !self.name.chars().any(char::is_control),
            "Enter an image source name up to 100 bytes."
        );
        self.endpoint = endpoint(self.endpoint.trim())?.to_string();
        self.model = self.model.trim().to_owned();
        ensure!(
            self.model.len() <= 300 && !self.model.chars().any(char::is_control),
            "Invalid image model ID."
        );
        if matches!(
            self.kind,
            SourceKind::OpenAi | SourceKind::Gemini | SourceKind::Imagen | SourceKind::Replicate
        ) {
            ensure!(!self.model.is_empty(), "Enter an image model ID.");
        }
        ensure!(
            serde_json::to_vec(&self.options)?.len() <= MAX_OPTIONS,
            "Image options or workflow exceed 512 KiB."
        );
        self.output_pointer = self.output_pointer.trim().to_owned();
        ensure!(
            self.output_pointer.len() <= 512
                && (self.output_pointer.is_empty() || self.output_pointer.starts_with('/')),
            "Use a JSON Pointer such as /images/0/url."
        );
        self.key_header = self.key_header.trim().to_ascii_lowercase();
        validate_request_header(self.auth_header())?;
        let mut names = HashSet::new();
        for (name, variable) in &mut self.headers {
            *name = name.trim().to_ascii_lowercase();
            *variable = variable.trim().to_owned();
            validate_request_header(name)?;
            validate_env(variable)?;
            ensure!(
                names.insert(name.clone()),
                "Duplicate image request header."
            );
        }
        ensure!(
            self.headers.len() <= 20 && self.asset_hosts.len() <= 20,
            "Add up to 20 image headers and asset hosts."
        );
        for host in &mut self.asset_hosts {
            *host = host.trim().to_ascii_lowercase();
            let suffix = host.strip_prefix("*.").unwrap_or(host);
            ensure!(
                suffix.contains('.')
                    && suffix.len() <= 253
                    && suffix.split('.').all(|label| !label.is_empty()
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')),
                "Enter asset hostnames such as cdn.example.com or *.example.com."
            );
        }
        if self.kind == SourceKind::ComfyUi && !self.options.is_empty() {
            validate_workflow(&self.options)?;
        }
        if self.kind == SourceKind::Custom && !self.options.is_empty() {
            ensure!(
                self.options.values().any(|value| prompt_slots(value) > 0),
                "Put {{prompt}} in a string value in the custom JSON body."
            );
        }
        Ok(())
    }

    pub fn config(&self) -> Config {
        Config {
            provider: self.id,
            model: if self.model.is_empty() {
                self.name.clone()
            } else {
                self.model.clone()
            },
            size: None,
            quality: None,
            request_base64: false,
        }
    }

    pub fn ready_to_generate(&self) -> bool {
        self.kind != SourceKind::ComfyUi || !self.options.is_empty()
    }

    pub fn credential_id(&self) -> Uuid {
        self.secret_ref.unwrap_or(self.id)
    }

    pub fn secret(&self) -> Result<Option<String>> {
        read_secret(self.credential_id(), &self.endpoint, self.has_secret)
    }

    pub fn same_key_target(&self, old: &Self) -> bool {
        self.endpoint == old.endpoint && self.kind == old.kind && self.key_header == old.key_header
    }

    fn auth_header(&self) -> &str {
        if !self.key_header.is_empty() {
            return &self.key_header;
        }
        match self.kind {
            SourceKind::Gemini | SourceKind::Imagen => "x-goog-api-key",
            SourceKind::Ideogram => "api-key",
            SourceKind::BlackForestLabs => "x-key",
            _ => "authorization",
        }
    }

    pub(super) fn authorized(
        &self,
        request: RequestBuilder,
        secret: Option<&str>,
    ) -> Result<RequestBuilder> {
        let mut request = request;
        if let Some(secret) = secret {
            let header_name = self.auth_header();
            ensure!(
                !self.headers.iter().any(|(name, _)| name == header_name),
                "Use one credential source for the image API key header."
            );
            let credential = match (self.key_header.is_empty(), header_name, self.kind) {
                (true, "authorization", SourceKind::Fal) => format!("Key {secret}"),
                (true, "authorization", _) => format!("Bearer {secret}"),
                _ => secret.to_owned(),
            };
            request = request.header(header_name, sensitive_header(&credential)?);
        }
        for (name, variable) in &self.headers {
            let value = std::env::var(variable)
                .with_context(|| format!("Set {variable} in the OpenDex environment."))?;
            request = request.header(name.as_str(), sensitive_header(&value)?);
        }
        Ok(request.timeout(Duration::from_secs(120)))
    }
}

pub(super) fn validate_workflow(workflow: &Map<String, Value>) -> Result<()> {
    ensure!(
        !workflow.is_empty()
            && workflow
                .values()
                .all(|node| { node["class_type"].is_string() && node["inputs"].is_object() }),
        "Use ComfyUI's API-format workflow JSON."
    );
    ensure!(
        workflow
            .values()
            .any(|node| prompt_slots(&node["inputs"]) > 0),
        "Put {{prompt}} in a text input in the ComfyUI workflow."
    );
    Ok(())
}

fn sensitive_header(value: &str) -> Result<header::HeaderValue> {
    let mut value = header::HeaderValue::from_str(value)
        .context("An image credential is not valid for an HTTP header.")?;
    value.set_sensitive(true);
    Ok(value)
}

fn prompt_slots(value: &Value) -> usize {
    match value {
        Value::String(value) => value.matches(PROMPT).count(),
        Value::Array(items) => items.iter().map(prompt_slots).sum(),
        Value::Object(items) => items.values().map(prompt_slots).sum(),
        _ => 0,
    }
}

fn insert_prompt(value: &mut Value, prompt: &str) {
    match value {
        Value::String(value) if value.contains(PROMPT) => *value = value.replace(PROMPT, prompt),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|value| insert_prompt(value, prompt)),
        Value::Object(items) => items
            .values_mut()
            .for_each(|value| insert_prompt(value, prompt)),
        _ => {}
    }
}

pub async fn generate_source(client: &Client, source: &Source, prompt: &str) -> Result<Attachment> {
    ensure!(
        source.ready_to_generate(),
        "Choose an API workflow for this ComfyUI connection."
    );
    ensure!(
        !prompt.trim().is_empty() && prompt.len() <= 32_000,
        "Enter an image prompt up to 32,000 bytes."
    );
    let key = source.secret()?;
    let request = ImageRequest {
        client,
        source,
        key: key.as_deref(),
        base: endpoint(&source.endpoint)?,
    };
    let bytes = tokio::time::timeout(Duration::from_secs(360), request.generate(prompt))
        .await
        .context("Image generation did not finish within six minutes.")??;
    attachment(bytes)
}

struct ImageRequest<'a> {
    client: &'a Client,
    source: &'a Source,
    key: Option<&'a str>,
    base: Url,
}

impl ImageRequest<'_> {
    async fn generate(&self, prompt: &str) -> Result<Vec<u8>> {
        let mut options = Value::Object(self.source.options.clone());
        if matches!(self.source.kind, SourceKind::ComfyUi | SourceKind::Custom) {
            ensure!(
                prompt_slots(&options).saturating_mul(prompt.len()) <= 2 * MAX_OPTIONS,
                "The rendered image request is too large."
            );
        }
        let model = self.source.model.as_str();
        match self.source.kind {
            SourceKind::OpenAi => {
                options["model"] = json!(model);
                options["prompt"] = json!(prompt);
                let body = self.post(self.base.clone(), &options).await?;
                self.output(&body, &["/data/0/b64_json", "/data/0/url"])
                    .await
            }
            SourceKind::Gemini | SourceKind::Imagen => self.google(model, prompt, options).await,
            SourceKind::Stability | SourceKind::Ideogram => {
                options["prompt"] = json!(prompt);
                let mut form = multipart::Form::new();
                for (name, value) in options.as_object().context("Invalid image options.")? {
                    ensure!(
                        value.is_string() || value.is_number() || value.is_boolean(),
                        "Image form fields must be text, numbers, or booleans."
                    );
                    let value = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned);
                    form = form.text(name.clone(), value);
                }
                let request = self.client.post(self.base.clone()).multipart(form);
                if self.source.kind == SourceKind::Stability {
                    return limited_body(
                        self.send(request.header(header::ACCEPT, "image/*")).await?,
                        MAX_IMAGE_BYTES,
                    )
                    .await;
                }
                let body = self.json(request).await?;
                self.output(&body, &["/data/0/url"]).await
            }
            SourceKind::ComfyUi => self.comfy(prompt, options).await,
            SourceKind::Replicate => self.replicate(model, prompt, options).await,
            SourceKind::Fal | SourceKind::BlackForestLabs | SourceKind::Firefly => {
                options["prompt"] = json!(prompt);
                self.provider_job(&options).await
            }
            SourceKind::Custom => {
                if self.source.options.is_empty() {
                    options["prompt"] = json!(prompt);
                } else {
                    insert_prompt(&mut options, prompt);
                }
                let bytes = limited_body(
                    self.send(self.client.post(self.base.clone()).json(&options))
                        .await?,
                    MAX_JSON_BYTES,
                )
                .await?;
                if bytes.len() <= MAX_IMAGE_BYTES && raster_mime(&bytes).is_some() {
                    return Ok(bytes);
                }
                let body = serde_json::from_slice(&bytes)
                    .context("The custom endpoint returned neither an image nor valid JSON.")?;
                self.output(
                    &body,
                    &[
                        "/data/0/b64_json",
                        "/data/0/url",
                        "/images/0/url",
                        "/image",
                        "/output/0",
                        "/output",
                        "/url",
                    ],
                )
                .await
            }
        }
    }

    async fn google(&self, model: &str, prompt: &str, options: Value) -> Result<Vec<u8>> {
        let model = model.strip_prefix("models/").unwrap_or(model);
        ensure!(
            !model.is_empty()
                && model
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
            "Use a valid Google image model ID."
        );
        if self.source.kind == SourceKind::Gemini {
            let mut config = json!({"responseModalities":["TEXT","IMAGE"]});
            config
                .as_object_mut()
                .unwrap()
                .extend(options.as_object().unwrap().clone());
            let body = self
                .post(
                    append(&self.base, &["models", &format!("{model}:generateContent")])?,
                    &json!({"contents":[{"parts":[{"text":prompt}]}],"generationConfig":config}),
                )
                .await?;
            let data = body["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|candidate| candidate["content"]["parts"].as_array())
                .flatten()
                .find_map(|part| {
                    part.pointer("/inlineData/data")
                        .or_else(|| part.pointer("/inline_data/data"))
                        .and_then(Value::as_str)
                })
                .context("Gemini returned no image. Check the selected model and prompt.")?;
            decode(data)
        } else {
            let mut parameters = json!({"sampleCount":1});
            parameters
                .as_object_mut()
                .unwrap()
                .extend(options.as_object().unwrap().clone());
            let body = self
                .post(
                    append(&self.base, &["models", &format!("{model}:predict")])?,
                    &json!({"instances":[{"prompt":prompt}],"parameters":parameters}),
                )
                .await?;
            self.output(&body, &["/predictions/0/bytesBase64Encoded"])
                .await
        }
    }

    async fn provider_job(&self, options: &Value) -> Result<Vec<u8>> {
        let job = self.post(self.base.clone(), options).await?;
        let (poll_field, pending, ready): (&str, &[&str], &[&str]) = match self.source.kind {
            SourceKind::Fal => ("status_url", &["IN_QUEUE", "IN_PROGRESS"], &["COMPLETED"]),
            SourceKind::BlackForestLabs => ("polling_url", &["Pending", "Processing"], &["Ready"]),
            SourceKind::Firefly => (
                "statusUrl",
                &["running", "pending", "queued", "not_started"],
                &["succeeded"],
            ),
            _ => unreachable!("only queued providers have a job"),
        };
        let poll = self.job_url(&job, poll_field)?;
        let finished = self.poll(poll, pending, ready).await?;
        ensure!(
            finished["error"].is_null(),
            "The image provider reported a failed job."
        );
        match self.source.kind {
            SourceKind::Fal => {
                let result = self.job_url(&job, "response_url")?;
                let body = self.json(self.client.get(result)).await?;
                self.output(&body, &["/images/0/url", "/image/url"]).await
            }
            SourceKind::BlackForestLabs => self.output(&finished, &["/result/sample"]).await,
            SourceKind::Firefly => {
                self.output(&finished, &["/result/outputs/0/image/url"])
                    .await
            }
            _ => unreachable!("only queued providers have a result"),
        }
    }

    async fn replicate(&self, model: &str, prompt: &str, mut input: Value) -> Result<Vec<u8>> {
        input["prompt"] = json!(prompt);
        let (url, payload) = if let Some((_, version)) = model.rsplit_once(':') {
            ensure!(
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Use an owner/model ID or owner/model:version for Replicate."
            );
            (
                append(&self.base, &["predictions"])?,
                json!({"version":version,"input":input}),
            )
        } else {
            let (owner, name) = model
                .split_once('/')
                .context("Use an owner/model ID for Replicate.")?;
            ensure!(
                [owner, name].into_iter().all(|part| !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))),
                "Use a valid owner/model ID for Replicate."
            );
            (
                append(&self.base, &["models", owner, name, "predictions"])?,
                json!({"input":input}),
            )
        };
        let mut result = self
            .json(
                self.client
                    .post(url)
                    .header("Prefer", "wait=10")
                    .header("Cancel-After", "6m")
                    .json(&payload),
            )
            .await?;
        if result["status"] != "succeeded" {
            ensure!(
                matches!(result["status"].as_str(), Some("starting" | "processing")),
                "The Replicate image job failed."
            );
            let id = result["id"]
                .as_str()
                .context("Replicate returned no prediction ID.")?;
            result = self
                .poll(
                    append(&self.base, &["predictions", id])?,
                    &["starting", "processing"],
                    &["succeeded"],
                )
                .await?;
        }
        self.output(&result, &["/output/0", "/output"]).await
    }

    async fn comfy(&self, prompt: &str, mut workflow: Value) -> Result<Vec<u8>> {
        for node in workflow
            .as_object_mut()
            .context("Invalid ComfyUI workflow.")?
            .values_mut()
        {
            insert_prompt(&mut node["inputs"], prompt);
        }
        ensure!(
            serde_json::to_vec(&workflow)?.len() <= 2 * MAX_OPTIONS,
            "The rendered ComfyUI workflow is too large."
        );
        let job = self
            .post(
                append(&self.base, &["prompt"])?,
                &json!({"prompt":workflow,"client_id":Uuid::new_v4()}),
            )
            .await?;
        let id = job["prompt_id"]
            .as_str()
            .context("ComfyUI returned no prompt ID.")?;
        let history = append(&self.base, &["history", id])?;
        loop {
            let body = self.json(self.client.get(history.clone())).await?;
            if let Some(result) = body.get(id) {
                let view = comfy_image_view(&self.base, result)?;
                return limited_body(self.send(self.client.get(view)).await?, MAX_IMAGE_BYTES)
                    .await;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn poll(&self, url: Url, pending: &[&str], ready: &[&str]) -> Result<Value> {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let body = self.json(self.client.get(url.clone())).await?;
            let status = body["status"]
                .as_str()
                .context("The image job did not report a status.")?;
            if ready.contains(&status) {
                return Ok(body);
            }
            ensure!(
                pending.contains(&status),
                "The image job failed or reported an unexpected status."
            );
        }
    }

    fn job_url(&self, body: &Value, field: &str) -> Result<Url> {
        let value = body[field]
            .as_str()
            .context("The image provider did not return a job URL.")?;
        let url = self.base.join(value)?;
        let allowed = url.origin() == self.base.origin()
            || match self.source.kind {
                SourceKind::BlackForestLabs => official_bfl(&self.base) && official_bfl(&url),
                SourceKind::Firefly => official_firefly(&self.base) && official_firefly(&url),
                _ => false,
            };
        ensure!(
            allowed && clean_url(&url),
            "The image provider returned an untrusted job URL."
        );
        Ok(url)
    }

    async fn output(&self, body: &Value, defaults: &[&str]) -> Result<Vec<u8>> {
        let field = self.source.output_pointer.as_str();
        let value = if field.is_empty() {
            defaults.iter().find_map(|pointer| {
                body.pointer(pointer)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            })
        } else {
            body.pointer(field).and_then(Value::as_str)
        }
        .context("The image response did not contain a usable output image.")?;
        if let Some(data) = value.strip_prefix("data:image/") {
            let (_, encoded) = data
                .split_once(";base64,")
                .context("The endpoint returned an invalid image data URL.")?;
            return decode(encoded);
        }
        if value.starts_with("http://")
            || value.starts_with("https://")
            || (value.starts_with('/') && !value.starts_with("/9j/"))
        {
            let url = self.base.join(value)?;
            self.asset_url(&url)?;
            // Asset hosts never receive the generation credentials.
            let response = self
                .client
                .get(url)
                .timeout(Duration::from_secs(60))
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Could not download the generated image."))?;
            ensure!(
                response.status().is_success(),
                "The image download returned HTTP {}.",
                response.status()
            );
            return limited_body(response, MAX_IMAGE_BYTES).await;
        }
        decode(value)
    }

    fn asset_url(&self, url: &Url) -> Result<()> {
        let host = url.host_str().unwrap_or_default();
        let preset_domain = match self.source.kind {
            SourceKind::Fal if self.base.host_str() == Some("queue.fal.run") => Some("*.fal.media"),
            SourceKind::Replicate if self.base.host_str() == Some("api.replicate.com") => {
                Some("*.replicate.delivery")
            }
            SourceKind::BlackForestLabs if official_bfl(&self.base) => Some("*.bfl.ai"),
            SourceKind::Ideogram if self.base.host_str() == Some("api.ideogram.ai") => {
                Some("*.ideogram.ai")
            }
            SourceKind::Firefly if official_firefly(&self.base) => {
                Some("pre-signed-firefly-prod.s3-accelerate.amazonaws.com")
            }
            _ => None,
        };
        ensure!(
            clean_url(url)
                && (url.origin() == self.base.origin()
                    || (url.scheme() == "https"
                        && url.port().is_none()
                        && self
                            .source
                            .asset_hosts
                            .iter()
                            .map(String::as_str)
                            .chain(preset_domain)
                            .any(|pattern| matching_host(host, pattern)))),
            "The output image host {host} is not allowed. Add its HTTPS hostname in the image source settings."
        );
        Ok(())
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        providers::checked(
            self.source
                .authorized(request, self.key)?
                .send()
                .await
                .map_err(|error| error.without_url())?,
        )
        .await
    }

    async fn json(&self, request: RequestBuilder) -> Result<Value> {
        let bytes = limited_body(self.send(request).await?, MAX_JSON_BYTES).await?;
        serde_json::from_slice(&bytes).context("The image endpoint returned invalid JSON.")
    }

    async fn post(&self, url: Url, payload: &Value) -> Result<Value> {
        self.json(self.client.post(url).json(payload)).await
    }
}

fn comfy_image_view(base: &Url, result: &Value) -> Result<Url> {
    let image = result["outputs"]
        .as_object()
        .into_iter()
        .flat_map(|outputs| outputs.values())
        .filter_map(|node| node["images"].as_array())
        .flatten()
        .find(|image| image["type"] == "output")
        .context("ComfyUI did not return a saved image. Add a Save Image node to the workflow.")?;
    let filename = image["filename"]
        .as_str()
        .context("ComfyUI returned no image filename.")?;
    let mut view = append(base, &["view"])?;
    view.query_pairs_mut()
        .append_pair("filename", filename)
        .append_pair("subfolder", image["subfolder"].as_str().unwrap_or(""))
        .append_pair("type", "output");
    Ok(view)
}

fn append(base: &Url, segments: &[&str]) -> Result<Url> {
    ensure!(
        segments.iter().all(|segment| !segment.is_empty()
            && !matches!(*segment, "." | "..")
            && !segment.contains(['/', '\\'])),
        "Invalid image request path."
    );
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid image API base URL."))?
        .pop_if_empty()
        .extend(segments.iter().copied());
    Ok(url)
}

fn clean_url(url: &Url) -> bool {
    matches!(url.scheme(), "https" | "http")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn matching_host(host: &str, pattern: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
    } else {
        host == pattern
    }
}

fn official_bfl(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            host == "api.bfl.ai" || (host.starts_with("api.") && host.ends_with(".bfl.ai"))
        })
}

fn official_firefly(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            host == "firefly-api.adobe.io"
                || (host.starts_with("firefly-") && host.ends_with(".adobe.io"))
        })
}
