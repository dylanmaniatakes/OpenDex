//! Dictation settings and endpoint adapters. Microphone capture is desktop-only.
use crate::{
    providers,
    state::{endpoint, read_secret, validate_request_header},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, header, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[cfg(feature = "desktop")]
mod local;
#[cfg(feature = "desktop")]
pub(crate) use local::{EngineChoice, ModelStatus, Recorder, download_model, model_status};

pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_SECONDS: u32 = 120;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Local,
    Endpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    OpenAi,
    OpenAiCompatible,
    ElevenLabs,
    Hermes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: Uuid,
    pub kind: EndpointKind,
    pub url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub key_header: String,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<Uuid>,
}

impl Endpoint {
    pub fn credential_id(&self) -> Uuid {
        self.secret_ref.unwrap_or(self.id)
    }

    pub fn secret(&self) -> Result<Option<String>> {
        read_secret(self.credential_id(), &self.url, self.has_secret)
    }

    pub fn same_key_target(&self, other: &Self) -> bool {
        self.url == other.url && self.kind == other.kind && self.key_header == other.key_header
    }

    fn validate(&mut self) -> Result<()> {
        let url = endpoint(self.url.trim())?;
        self.url = url.to_string();
        self.model = self.model.trim().to_owned();
        ensure!(
            self.model.len() <= 300
                && !self.model.chars().any(char::is_control)
                && (!self.model.is_empty() || self.kind == EndpointKind::Hermes),
            "Enter an audio transcription model ID."
        );
        let official = match self.kind {
            EndpointKind::OpenAi => Some(("api.openai.com", "/v1/audio/transcriptions")),
            EndpointKind::ElevenLabs => Some(("api.elevenlabs.io", "/v1/speech-to-text")),
            _ => None,
        };
        if let Some((host, path)) = official {
            ensure!(
                url.scheme() == "https"
                    && url.host_str() == Some(host)
                    && url.port().is_none()
                    && url.path() == path,
                "Use the official transcription URL or choose OpenAI-compatible."
            );
        }
        self.key_header = match self.kind {
            EndpointKind::OpenAi => String::new(),
            EndpointKind::ElevenLabs => "xi-api-key".into(),
            EndpointKind::Hermes if self.key_header.trim().is_empty() => {
                "x-hermes-session-token".into()
            }
            _ => self.key_header.trim().to_ascii_lowercase(),
        };
        if !self.key_header.is_empty() {
            validate_request_header(&self.key_header)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    pub local_model: String,
    pub custom_model: Option<PathBuf>,
    pub language: String,
    pub endpoint: Option<Endpoint>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            local_model: "base-q5_1".into(),
            custom_model: None,
            language: String::new(),
            endpoint: None,
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        self.mode == Mode::Off
            && self.local_model == "base-q5_1"
            && self.custom_model.is_none()
            && self.language.is_empty()
            && self.endpoint.is_none()
    }

    pub fn validate(&mut self) -> Result<()> {
        self.local_model = self.local_model.trim().to_owned();
        ensure!(
            self.local_model == "custom" || MODELS.iter().any(|model| model.id == self.local_model),
            "Choose a local Whisper model."
        );
        if self.mode == Mode::Local && self.local_model == "custom" {
            ensure!(
                self.custom_model
                    .as_deref()
                    .is_some_and(std::path::Path::is_absolute),
                "Choose an existing Whisper GGML model file."
            );
        }
        self.language = self.language.trim().to_ascii_lowercase();
        ensure!(
            matches!(self.language.len(), 0 | 2 | 3)
                && self.language.bytes().all(|byte| byte.is_ascii_lowercase()),
            "Enter a two- or three-letter language code or leave it blank."
        );
        if let Some(endpoint) = self.endpoint.as_mut() {
            endpoint.validate()?;
        }
        ensure!(
            self.mode != Mode::Endpoint || self.endpoint.is_some(),
            "Set a transcription endpoint."
        );
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Model {
    pub id: &'static str,
    pub label: &'static str,
    pub bytes: u64,
    #[serde(skip)]
    pub sha256: &'static str,
}

pub const MODEL_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";
pub const MODELS: &[Model] = &[
    Model {
        id: "base-q5_1",
        label: "Whisper Base · multilingual · quantized",
        bytes: 59_707_625,
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
    },
    Model {
        id: "base.en-q5_1",
        label: "Whisper Base · English · quantized",
        bytes: 59_721_011,
        sha256: "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
    },
    Model {
        id: "tiny",
        label: "Whisper Tiny · multilingual",
        bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    Model {
        id: "tiny.en",
        label: "Whisper Tiny · English",
        bytes: 77_704_715,
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    },
    Model {
        id: "small-q5_1",
        label: "Whisper Small · multilingual · quantized",
        bytes: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    },
    Model {
        id: "medium-q5_0",
        label: "Whisper Medium · multilingual · quantized",
        bytes: 539_212_467,
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
    },
    Model {
        id: "large-v3-turbo-q5_0",
        label: "Whisper Large V3 Turbo · multilingual · quantized",
        bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
];

pub fn wav(samples: &[f32]) -> Vec<u8> {
    let length = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + length as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(length + 36).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&length.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(
            &((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).to_le_bytes(),
        );
    }
    out
}

pub async fn remote(
    client: &Client,
    config: &Endpoint,
    secret: Option<&str>,
    language: &str,
    samples: &[f32],
) -> Result<String> {
    ensure!(
        !samples.is_empty() && samples.len() <= (MAX_SECONDS * SAMPLE_RATE) as usize,
        "The microphone recording is empty or too long."
    );
    if matches!(config.kind, EndpointKind::OpenAi | EndpointKind::ElevenLabs) {
        ensure!(
            secret.is_some(),
            "Save an API key for this transcription endpoint."
        );
    }
    let audio = wav(samples);
    let request = client.post(endpoint(&config.url)?);
    let mut request = if config.kind == EndpointKind::Hermes {
        request.json(&json!({
            "data_url": format!("data:audio/wav;base64,{}", STANDARD.encode(audio)),
            "mime_type": "audio/wav"
        }))
    } else {
        let model_field = if config.kind == EndpointKind::ElevenLabs {
            "model_id"
        } else {
            "model"
        };
        let part = multipart::Part::bytes(audio)
            .file_name("dictation.wav")
            .mime_str("audio/wav")?;
        let mut form = multipart::Form::new()
            .text(model_field, config.model.clone())
            .part("file", part);
        if !language.is_empty() {
            let key = if config.kind == EndpointKind::ElevenLabs {
                "language_code"
            } else {
                "language"
            };
            form = form.text(key, language.to_owned());
        }
        request.multipart(form)
    };
    if let Some(secret) = secret {
        if config.key_header.is_empty() {
            request = request.bearer_auth(secret);
        } else {
            let mut value = header::HeaderValue::from_str(secret)
                .context("The transcription credential is invalid.")?;
            value.set_sensitive(true);
            request = request.header(config.key_header.as_str(), value);
        }
    }
    let response = request
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|error| error.without_url())?;
    let body = providers::bounded_text(providers::checked(response).await?, 1024 * 1024).await?;
    let result: Value =
        serde_json::from_str(&body).context("The transcription endpoint did not return JSON.")?;
    let field = if config.kind == EndpointKind::Hermes {
        "transcript"
    } else {
        "text"
    };
    Ok(result[field]
        .as_str()
        .context("The response did not contain a transcript.")?
        .trim()
        .to_owned())
}
