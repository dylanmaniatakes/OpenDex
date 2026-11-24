//! Image generation uses a user-selected HTTP provider independently of the chat model.

use crate::{
    providers,
    state::{Attachment, Provider, ProviderKind, endpoint, normalize_models},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{io::Write as _, path::Path, time::Duration};
use uuid::Uuid;

const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENCODED_BYTES: usize = MAX_IMAGE_BYTES.div_ceil(3) * 4;
const MAX_JSON_BYTES: usize = MAX_ENCODED_BYTES + 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub provider: Uuid,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default)]
    pub request_base64: bool,
}

impl Config {
    pub fn validate(&mut self) -> Result<()> {
        let mut models = vec![std::mem::take(&mut self.model)];
        normalize_models(&mut models)?;
        self.model = models.pop().context("Enter an image model ID.")?;
        self.size = self
            .size
            .take()
            .map(|size| size.trim().to_ascii_lowercase())
            .filter(|size| !size.is_empty());
        if let Some(size) = self.size.as_deref().filter(|size| *size != "auto") {
            let valid = |part: &str| {
                part.parse::<u16>()
                    .is_ok_and(|pixels| (1..=8192).contains(&pixels))
            };
            ensure!(
                size.split_once('x')
                    .is_some_and(|(width, height)| valid(width) && valid(height)),
                "Enter image dimensions as WIDTHxHEIGHT or leave the size blank."
            );
        }
        self.quality = self
            .quality
            .take()
            .map(|quality| quality.trim().to_ascii_lowercase())
            .filter(|quality| !quality.is_empty());
        if let Some(quality) = self.quality.as_deref() {
            ensure!(
                matches!(
                    quality,
                    "auto" | "low" | "medium" | "high" | "xhigh" | "max" | "standard" | "hd"
                ),
                "Choose a supported image quality."
            );
        }
        Ok(())
    }

    pub fn resolve<'a>(
        &self,
        providers: impl IntoIterator<Item = &'a Provider>,
    ) -> Result<&'a Provider> {
        providers
            .into_iter()
            .find(|item| {
                item.id == self.provider
                    && matches!(
                        item.kind,
                        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible
                    )
            })
            .context("Choose an OpenAI API or OpenAI-compatible image connection.")
    }
}

#[derive(Deserialize)]
struct ImageResponse {
    data: Vec<GeneratedImage>,
}

#[derive(Deserialize)]
struct GeneratedImage {
    b64_json: Option<String>,
    url: Option<String>,
}

pub async fn generate(
    client: &Client,
    provider: &Provider,
    config: &Config,
    prompt: &str,
) -> Result<Attachment> {
    ensure!(
        !prompt.trim().is_empty() && prompt.len() <= 32_000,
        "Enter an image prompt up to 32,000 bytes."
    );
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid image API endpoint."))?
        .pop_if_empty()
        .extend(["images", "generations"]);
    let mut payload = json!({"model":config.model,"prompt":prompt});
    if let Some(size) = &config.size {
        payload["size"] = json!(size);
    }
    if let Some(quality) = &config.quality {
        payload["quality"] = json!(quality);
    }
    if provider.kind == ProviderKind::OpenAiCompatible && config.request_base64 {
        payload["response_format"] = json!("b64_json");
    }
    let request = providers::request(provider, client.post(url.clone()).json(&payload))?;
    let response = request.timeout(Duration::from_secs(360)).send().await?;
    let body = limited_body(providers::checked(response).await?, MAX_JSON_BYTES).await?;
    let response: ImageResponse = serde_json::from_slice(&body)
        .context("The image endpoint returned an invalid response.")?;
    let image = response
        .data
        .into_iter()
        .next()
        .context("The image endpoint returned no images.")?;
    let bytes = if let Some(encoded) = image.b64_json.filter(|value| !value.is_empty()) {
        decode(&encoded)?
    } else {
        let asset = image
            .url
            .context("The image endpoint returned neither Base64 data nor an image URL.")?;
        let url = same_origin_asset(&url, &asset)?;
        // Asset retrieval never receives the model provider's key or custom headers.
        let response = client
            .get(url)
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "The image download returned HTTP {}.",
            response.status()
        );
        limited_body(response, MAX_IMAGE_BYTES).await?
    };
    let mime = raster_mime(&bytes)
        .context("The endpoint did not return a PNG, JPEG, WebP, or GIF image.")?;
    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "gif",
    };
    Ok(Attachment {
        name: format!("OpenDex-{}.{}", Uuid::new_v4().simple(), extension),
        text: STANDARD.encode(bytes),
        media_type: Some(mime.into()),
    })
}

fn same_origin_asset(endpoint: &Url, asset: &str) -> Result<Url> {
    let asset = endpoint
        .join(asset)
        .context("The endpoint returned an invalid image URL.")?;
    ensure!(
        asset.origin() == endpoint.origin()
            && asset.username().is_empty()
            && asset.password().is_none(),
        "The endpoint returned an image URL on another origin. Configure it to return Base64 image data."
    );
    Ok(asset)
}

async fn limited_body(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "The image response exceeds the size limit."
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= limit,
            "The image response exceeds the size limit."
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn decode(encoded: &str) -> Result<Vec<u8>> {
    ensure!(
        encoded.len() <= MAX_ENCODED_BYTES,
        "The image exceeds the 16 MiB limit."
    );
    let bytes = STANDARD
        .decode(encoded)
        .context("The endpoint returned invalid Base64 data.")?;
    ensure!(
        bytes.len() <= MAX_IMAGE_BYTES,
        "The image exceeds the 16 MiB limit."
    );
    Ok(bytes)
}

pub(crate) fn raster_mime(header: &[u8]) -> Option<&'static str> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if header.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

pub fn save(attachment: &Attachment, path: &Path) -> Result<()> {
    let bytes = decode(&attachment.text)?;
    let mime = raster_mime(&bytes).context("The saved image is invalid.")?;
    ensure!(
        attachment.media_type.as_deref() == Some(mime),
        "The saved image format does not match."
    );
    let directory = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&bytes)?;
    file.persist(path)?;
    Ok(())
}
