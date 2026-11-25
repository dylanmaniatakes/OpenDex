//! Speech synthesis runs independently of the conversation's model provider.
use crate::state::{endpoint, read_secret};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{
    Client, Response, Url,
    header::{AUTHORIZATION, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

const MAX_AUDIO: usize = 16 * 1024 * 1024;
const MAX_ENCODED_AUDIO: usize = MAX_AUDIO.div_ceil(3) * 4;
const MAX_SPEECH_CHARS: usize = 3500;
pub const SAMPLE: &str = "This is a preview of your OpenDex voice.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    OpenAi,
    OpenAiCompatible,
    ElevenLabs,
    PocketTts,
    Piper,
    OmniVoice,
    Kokoro,
    Cartesia,
    Deepgram,
    Hermes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub kind: Kind,
    pub endpoint: String,
    pub model: String,
    pub voice: String,
    #[serde(default)]
    pub key_header: String,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<Uuid>,
}

impl Profile {
    pub fn validate(&mut self) -> Result<()> {
        for field in [
            &mut self.name,
            &mut self.endpoint,
            &mut self.model,
            &mut self.voice,
            &mut self.key_header,
        ] {
            *field = field.trim().to_owned();
            ensure!(
                !field.chars().any(char::is_control),
                "Voice settings cannot contain control characters."
            );
        }
        ensure!(
            !self.name.is_empty() && self.name.len() <= 120,
            "Enter a voice connection name up to 120 bytes."
        );
        ensure!(
            self.model.len() <= 300 && self.voice.len() <= 1000,
            "The speech model or voice ID is too long."
        );
        let url = endpoint(&self.endpoint)?;
        match self.kind {
            Kind::OpenAi => {
                ensure!(
                    is_official(&url, "api.openai.com", "/v1/audio/speech"),
                    "Use the official OpenAI speech URL or choose OpenAI-compatible."
                );
            }
            Kind::ElevenLabs => {
                ensure!(
                    is_official(&url, "api.elevenlabs.io", "/"),
                    "Use the official ElevenLabs API URL."
                );
                ensure!(
                    self.voice
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'),
                    "Enter a valid ElevenLabs voice ID."
                );
            }
            Kind::Cartesia => ensure!(
                is_official(&url, "api.cartesia.ai", "/tts/bytes"),
                "Use the official Cartesia speech URL."
            ),
            Kind::Deepgram => ensure!(
                is_official(&url, "api.deepgram.com", "/v1/speak"),
                "Use the official Deepgram speech URL."
            ),
            Kind::Piper => {
                ensure!(
                    self.model != "."
                        && self.model != ".."
                        && self.model.bytes().all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'_' | b'.')),
                    "Enter an installed Piper voice model name or leave it blank."
                )
            }
            Kind::OpenAiCompatible
            | Kind::PocketTts
            | Kind::OmniVoice
            | Kind::Kokoro
            | Kind::Hermes => {}
        }
        if matches!(
            self.kind,
            Kind::OpenAi
                | Kind::OpenAiCompatible
                | Kind::ElevenLabs
                | Kind::OmniVoice
                | Kind::Kokoro
                | Kind::Cartesia
        ) {
            ensure!(
                !self.model.is_empty() && !self.voice.is_empty(),
                "Enter a speech model and voice ID."
            );
        }
        if self.kind == Kind::Deepgram {
            ensure!(!self.model.is_empty(), "Enter a Deepgram voice model ID.");
        }
        self.endpoint = url.to_string();
        self.key_header = match self.kind {
            Kind::ElevenLabs => "xi-api-key".into(),
            Kind::Hermes if self.key_header.is_empty() => "x-hermes-session-token".into(),
            Kind::OpenAi | Kind::Cartesia | Kind::Deepgram => String::new(),
            _ => self.key_header.to_ascii_lowercase(),
        };
        if !self.key_header.is_empty() {
            HeaderName::try_from(self.key_header.as_str())
                .context("Enter a valid API key header name.")?;
            ensure!(
                !matches!(
                    self.key_header.as_str(),
                    "host" | "content-length" | "transfer-encoding" | "connection" | "content-type"
                ),
                "Choose a header intended for authentication."
            );
        }
        Ok(())
    }

    pub fn same_key_target(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.endpoint == other.endpoint
            && self.key_header == other.key_header
    }

    pub fn credential_id(&self) -> Uuid {
        self.secret_ref.unwrap_or(self.id)
    }

    pub fn secret(&self) -> Result<Option<String>> {
        read_secret(self.credential_id(), &self.endpoint, self.has_secret)
    }
}

fn is_official(url: &Url, host: &str, path: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port().is_none()
        && url.path() == path
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profiles: Vec<Profile>,
    pub selected: Option<Uuid>,
    pub autoplay: bool,
}

impl Settings {
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty() && self.selected.is_none() && !self.autoplay
    }

    pub fn profile(&self, id: Uuid) -> Result<&Profile> {
        self.profiles
            .iter()
            .find(|profile| profile.id == id)
            .context("That voice connection no longer exists.")
    }
}

#[derive(Serialize)]
pub struct Speech {
    pub mime: &'static str,
    pub base64: String,
    pub more: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Voice {
    #[serde(rename(deserialize = "voice_id"))]
    pub id: String,
    pub name: String,
}

pub fn speech_part(text: &str, part: usize) -> Result<(&str, bool)> {
    split_speech(text, part, MAX_SPEECH_CHARS)
}

fn split_speech(text: &str, part: usize, max_chars: usize) -> Result<(&str, bool)> {
    let mut remaining = text.trim();
    for index in 0..=part {
        ensure!(
            !remaining.is_empty(),
            "There is no speech text for this part."
        );
        let hard_end = remaining
            .char_indices()
            .nth(max_chars)
            .map(|(offset, _)| offset);
        let end = hard_end.map_or(remaining.len(), |offset| {
            remaining[..offset]
                .rfind(char::is_whitespace)
                .filter(|&boundary| boundary >= offset / 2)
                .unwrap_or(offset)
        });
        let (piece, rest) = remaining.split_at(end);
        remaining = rest.trim_start();
        if index == part {
            return Ok((piece.trim_end(), !remaining.is_empty()));
        }
    }
    unreachable!("the loop returns the requested part")
}

pub async fn synthesize(
    client: &Client,
    profile: &Profile,
    key: Option<&str>,
    text: &str,
    part: usize,
) -> Result<Speech> {
    let (text, more) = if profile.kind == Kind::Deepgram {
        split_speech(text, part, 2000)?
    } else {
        speech_part(text, part)?
    };
    let mut url = endpoint(&profile.endpoint)?;
    let request = match profile.kind {
        Kind::OpenAi | Kind::OpenAiCompatible | Kind::Kokoro => client.post(url).json(&json!({
            "model": profile.model, "voice": profile.voice, "input": text, "response_format": "mp3",
        })),
        Kind::OmniVoice => client.post(url).json(&json!({
            "model": profile.model, "voice": profile.voice, "input": text, "response_format": "wav",
        })),
        Kind::ElevenLabs => {
            url.path_segments_mut()
                .expect("HTTP base URL")
                .pop_if_empty()
                .extend(["v1", "text-to-speech", &profile.voice]);
            client
                .post(url)
                .query(&[("output_format", "mp3_44100_128")])
                .json(&json!({"text": text, "model_id": profile.model}))
        }
        Kind::PocketTts => {
            let mut fields = vec![("text", text)];
            if !profile.voice.is_empty() {
                fields.push(("voice_url", profile.voice.as_str()));
            }
            client.post(url).form(&fields)
        }
        Kind::Piper => {
            let mut body = json!({"text": text});
            if !profile.model.is_empty() { body["voice"] = json!(profile.model); }
            if !profile.voice.is_empty() {
                if let Ok(speaker) = profile.voice.parse::<u32>() {
                    body["speaker_id"] = json!(speaker);
                } else {
                    body["speaker"] = json!(profile.voice);
                }
            }
            client.post(url).json(&body)
        }
        Kind::Cartesia => client.post(url).header("cartesia-version", "2026-08-14").json(&json!({
            "model_id": profile.model, "transcript": text, "voice": profile.voice,
            "output_format": {"container": "wav", "encoding": "pcm_s16le", "sample_rate": 24000},
        })),
        Kind::Deepgram => client.post(url).query(&[("model", profile.model.as_str()), ("encoding", "linear16"), ("container", "wav")])
            .json(&json!({"text": text})),
        Kind::Hermes => client.post(url).json(&json!({"text": text})),
    };
    let response = send(profile, key, request, Duration::from_secs(300)).await?;
    let (mime, base64) = if profile.kind == Kind::Hermes {
        #[derive(Deserialize)]
        struct HermesAudio {
            data_url: String,
        }
        let audio: HermesAudio =
            serde_json::from_slice(&body(response, MAX_ENCODED_AUDIO + 4096).await?)
                .context("Hermes returned an invalid speech response.")?;
        let (mime, encoded) = audio
            .data_url
            .strip_prefix("data:")
            .and_then(|url| url.split_once(";base64,"))
            .context("Hermes did not return Base64 audio.")?;
        ensure!(
            encoded.len() <= MAX_ENCODED_AUDIO,
            "The generated audio exceeds 16 MiB."
        );
        let bytes = STANDARD
            .decode(encoded)
            .context("Hermes returned invalid Base64 audio.")?;
        ensure!(
            bytes.len() <= MAX_AUDIO,
            "The generated audio exceeds 16 MiB."
        );
        (audio_type(mime, &bytes)?, encoded.to_owned())
    } else {
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
        let bytes = body(response, MAX_AUDIO).await?;
        (audio_type(&mime, &bytes)?, STANDARD.encode(bytes))
    };
    Ok(Speech { mime, base64, more })
}

pub async fn elevenlabs_voices(
    client: &Client,
    profile: &Profile,
    key: Option<&str>,
    search: &str,
) -> Result<Vec<Voice>> {
    ensure!(
        profile.kind == Kind::ElevenLabs,
        "Voice discovery is available for ElevenLabs."
    );
    #[derive(Deserialize)]
    struct Catalog {
        voices: Vec<Voice>,
    }

    let mut url = endpoint(&profile.endpoint)?;
    url.set_path("/v2/voices");
    let request = client
        .get(url)
        .query(&[("page_size", "100"), ("search", search)]);
    let response = send(profile, key, request, Duration::from_secs(20)).await?;
    let list: Catalog = serde_json::from_slice(&body(response, 2 * 1024 * 1024).await?)
        .context("ElevenLabs returned an invalid voice list.")?;
    Ok(list.voices)
}

async fn send(
    profile: &Profile,
    key: Option<&str>,
    request: reqwest::RequestBuilder,
    timeout: Duration,
) -> Result<Response> {
    if matches!(
        profile.kind,
        Kind::OpenAi | Kind::ElevenLabs | Kind::Deepgram | Kind::Cartesia
    ) {
        ensure!(key.is_some(), "Save an API key for this voice connection.");
    }
    let request = match (profile.kind, key, profile.key_header.as_str()) {
        (Kind::Deepgram, Some(key), _) => {
            let mut value = HeaderValue::from_str(&format!("Token {key}"))
                .context("The saved voice key is invalid.")?;
            value.set_sensitive(true);
            request.header(AUTHORIZATION, value)
        }
        (_, Some(key), "") => request.bearer_auth(key),
        (_, Some(key), header) => {
            let mut value =
                HeaderValue::from_str(key).context("The saved voice key is invalid.")?;
            value.set_sensitive(true);
            request.header(header, value)
        }
        (_, None, _) => request,
    };
    let response = request
        .timeout(timeout)
        .send()
        .await
        .context("Could not reach the voice endpoint.")?;
    ensure!(
        response.status().is_success(),
        "The voice endpoint returned HTTP {}.",
        response.status()
    );
    Ok(response)
}

async fn body(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "The speech response exceeds the size limit."
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "The speech response exceeds the size limit."
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn audio_type(mime: &str, bytes: &[u8]) -> Result<&'static str> {
    ensure!(!bytes.is_empty(), "The voice endpoint returned no audio.");
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        return Ok("audio/wav");
    }
    if bytes.starts_with(b"ID3")
        || bytes
            .get(..2)
            .is_some_and(|header| header[0] == 0xff && header[1] & 0xe0 == 0xe0)
    {
        return Ok(if mime == "audio/aac" {
            "audio/aac"
        } else {
            "audio/mpeg"
        });
    }
    if bytes.starts_with(b"OggS") {
        return Ok("audio/ogg");
    }
    if bytes.starts_with(b"fLaC") {
        return Ok("audio/flac");
    }
    if bytes.get(4..8) == Some(b"ftyp") && matches!(mime, "audio/mp4" | "audio/x-m4a") {
        return Ok("audio/mp4");
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) && mime == "audio/webm" {
        return Ok("audio/webm");
    }
    bail!("The voice endpoint returned an unsupported audio format.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_parts_split_unicode_without_dropping_words() {
        let first = "声".repeat(MAX_SPEECH_CHARS - 2);
        let text = format!("  {first} another sentence.  ");
        assert_eq!(speech_part(&text, 0).unwrap(), (first.as_str(), true));
        assert_eq!(speech_part(&text, 1).unwrap(), ("another sentence.", false));
        assert!(speech_part(&text, 2).is_err());
        assert!(speech_part("  ", 0).is_err());
    }

    #[test]
    fn official_speech_connections_reject_other_origins() {
        let mut profile = Profile {
            id: Uuid::new_v4(),
            name: "OpenAI".into(),
            kind: Kind::OpenAi,
            endpoint: "https://api.openai.com.example.org/v1/audio/speech".into(),
            model: "gpt-4o-mini-tts".into(),
            voice: "marin".into(),
            key_header: String::new(),
            has_secret: false,
            secret_ref: None,
        };
        assert!(profile.validate().is_err());
        profile.endpoint = "https://api.openai.com/v1/audio/speech".into();
        profile.validate().unwrap();
        let mut redirected = profile.clone();
        redirected.kind = Kind::OpenAiCompatible;
        assert!(!profile.same_key_target(&redirected));
        assert!(audio_type("audio/wav", b"<html>fixture</html>").is_err());
    }
}
