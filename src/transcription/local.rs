use super::{Endpoint, MAX_SECONDS, MODEL_REVISION, MODELS, Mode, Model, SAMPLE_RATE, Settings};
use anyhow::{Context, Result, bail, ensure};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use reqwest::{Client, Url, redirect::Policy};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::oneshot};
use zeroize::Zeroizing;

pub(crate) enum EngineChoice {
    Local(PathBuf),
    Remote(Endpoint, Option<Zeroizing<String>>),
}

impl EngineChoice {
    pub(crate) fn resolve(settings: &Settings, data_dir: &Path) -> Result<Self> {
        match settings.mode {
            Mode::Off => bail!("Choose a dictation source in Settings."),
            Mode::Local => {
                let path = if settings.local_model == "custom" {
                    let file = settings
                        .custom_model
                        .as_ref()
                        .context("Choose a local Whisper GGML model file.")?;
                    let metadata =
                        fs::metadata(file).context("The selected Whisper model is unavailable.")?;
                    ensure!(
                        metadata.is_file() && metadata.len() <= 4 * 1024 * 1024 * 1024,
                        "Choose a Whisper model file up to 4 GiB."
                    );
                    file.clone()
                } else {
                    let model = lookup_model(&settings.local_model)?;
                    ensure!(
                        installed(data_dir, model),
                        "Download the selected Whisper model in Settings → Dictation first."
                    );
                    model_path(data_dir, model)
                };
                Ok(Self::Local(path))
            }
            Mode::Endpoint => {
                let config = settings
                    .endpoint
                    .clone()
                    .context("Configure a transcription endpoint first.")?;
                let secret = config.secret()?.map(Zeroizing::new);
                Ok(Self::Remote(config, secret))
            }
        }
    }

    pub(crate) async fn transcribe(
        self,
        client: &Client,
        language: String,
        samples: Vec<f32>,
    ) -> Result<String> {
        ensure!(
            samples.iter().any(|sample| sample.abs() > 0.0001),
            "No microphone audio was captured. Check the input device and microphone permissions."
        );
        match self {
            Self::Local(model) => {
                tokio::task::spawn_blocking(move || whisper(&model, &language, &samples)).await?
            }
            Self::Remote(config, key) => {
                super::remote(
                    client,
                    &config,
                    key.as_deref().map(String::as_str),
                    &language,
                    &samples,
                )
                .await
            }
        }
    }
}

pub(crate) struct Recorder {
    stop: mpsc::Sender<()>,
    worker: thread::JoinHandle<Result<Vec<f32>>>,
}

impl Recorder {
    pub(crate) async fn start() -> Result<Self> {
        microphone_permission().await?;
        let (started, ready) = oneshot::channel();
        let (stop, stopped) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("OpenDex microphone".into())
            .spawn(move || {
                let input = Input::start();
                match input {
                    Ok(input) => {
                        if started.send(Ok(())).is_err() {
                            bail!("Microphone recording cancelled.");
                        }
                        if matches!(
                            stopped.recv_timeout(Duration::from_secs(MAX_SECONDS.into())),
                            Err(mpsc::RecvTimeoutError::Disconnected)
                        ) {
                            bail!("Microphone recording cancelled.");
                        }
                        input.finish()
                    }
                    Err(error) => {
                        let _ = started.send(Err(error));
                        bail!("Could not start microphone recording.");
                    }
                }
            })?;
        ready.await.context("The microphone could not start.")??;
        Ok(Self { stop, worker })
    }

    pub(crate) async fn finish(self) -> Result<Vec<f32>> {
        let _ = self.stop.send(());
        let worker = self.worker;
        tokio::task::spawn_blocking(move || {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("The microphone capture ended unexpectedly."))?
        })
        .await?
    }
}

struct Input {
    stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    error: Arc<Mutex<Option<String>>>,
    sample_rate: u32,
}

impl Input {
    fn start() -> Result<Self> {
        let device = cpal::default_host()
            .default_input_device()
            .context("Choose a default microphone in your system sound settings.")?;
        let supported = device
            .default_input_config()
            .context("Could not read the default microphone format.")?;
        let sample_rate = supported.sample_rate().0;
        let config = supported.config();
        ensure!(
            sample_rate > 0 && sample_rate <= 192_000 && config.channels > 0,
            "Unsupported microphone sample rate or channel count."
        );
        let samples = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(None));
        let limit = (sample_rate * MAX_SECONDS) as usize;
        macro_rules! build {
            ($sample:ty) => {
                input_stream::<$sample>(
                    &device,
                    &config,
                    Arc::clone(&samples),
                    Arc::clone(&error),
                    limit,
                )
            };
        }
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build!(f32),
            SampleFormat::F64 => build!(f64),
            SampleFormat::I8 => build!(i8),
            SampleFormat::I16 => build!(i16),
            SampleFormat::I24 => build!(cpal::I24),
            SampleFormat::I32 => build!(i32),
            SampleFormat::I64 => build!(i64),
            SampleFormat::U8 => build!(u8),
            SampleFormat::U16 => build!(u16),
            SampleFormat::U32 => build!(u32),
            SampleFormat::U64 => build!(u64),
            _ => bail!("The default microphone uses an unsupported sample format."),
        }
        .context("Could not open the default microphone.")?;
        stream
            .play()
            .context("Could not start recording. Check system microphone permissions.")?;
        Ok(Self {
            stream,
            samples,
            error,
            sample_rate,
        })
    }

    fn finish(self) -> Result<Vec<f32>> {
        drop(self.stream);
        if let Some(error) = self.error.lock().expect("recording errors").take() {
            bail!("Microphone input failed: {error}");
        }
        let samples = std::mem::take(&mut *self.samples.lock().expect("microphone samples"));
        ensure!(
            !samples.is_empty(),
            "The microphone did not record any audio."
        );
        Ok(resample(samples, self.sample_rate))
    }
}

fn input_stream<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    error: Arc<Mutex<Option<String>>>,
    limit: usize,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut captured = samples.lock().expect("microphone samples");
            let remaining = limit.saturating_sub(captured.len());
            captured.extend(data.chunks_exact(channels).take(remaining).map(|frame| {
                frame
                    .iter()
                    .map(|sample| f32::from_sample(*sample))
                    .sum::<f32>()
                    / channels as f32
            }));
        },
        move |failure| *error.lock().expect("recording errors") = Some(failure.to_string()),
        None,
    )
}

fn resample(samples: Vec<f32>, input_rate: u32) -> Vec<f32> {
    if input_rate == SAMPLE_RATE {
        return samples;
    }
    let length = (samples.len() as u64 * u64::from(SAMPLE_RATE) / u64::from(input_rate)) as usize;
    let ratio = f64::from(input_rate) / f64::from(SAMPLE_RATE);
    (0..length)
        .map(|index| {
            let point = index as f64 * ratio;
            let before = point as usize;
            let mix = point.fract() as f32;
            let first = samples[before];
            let next = samples.get(before + 1).copied().unwrap_or(first);
            first + (next - first) * mix
        })
        .collect()
}

#[cfg(target_os = "macos")]
async fn microphone_permission() -> Result<()> {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    // AVMediaTypeAudio is the constant supplied by AVFoundation for microphone capture.
    let media =
        unsafe { AVMediaTypeAudio }.context("macOS microphone permissions are unavailable.")?;
    let permission = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if permission == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    let granted = if permission == AVAuthorizationStatus::NotDetermined {
        let (sender, receiver) = oneshot::channel();
        {
            let sender = Mutex::new(Some(sender));
            let callback = RcBlock::new(move |allowed: Bool| {
                if let Some(sender) = sender.lock().expect("microphone permission").take() {
                    let _ = sender.send(allowed.as_bool());
                }
            });
            // AVFoundation retains the completion block until it responds to the user.
            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &callback)
            };
        }
        receiver.await?
    } else {
        false
    };
    ensure!(
        granted,
        "Allow OpenDex microphone access in System Settings → Privacy & Security → Microphone."
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn microphone_permission() -> Result<()> {
    Ok(())
}

fn whisper(model: &Path, language: &str, pcm: &[f32]) -> Result<String> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let mut config = WhisperContextParameters::default();
    config.use_gpu(false);
    let context = WhisperContext::new_with_params(model, config)
        .context("Could not load the selected Whisper model.")?;
    let mut state = context.create_state()?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let threads = thread::available_parallelism().map_or(1, |cores| cores.get().min(6));
    params.set_n_threads(threads as i32);
    params.set_language(Some(if language.is_empty() {
        "auto"
    } else {
        language
    }));
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, pcm)
        .context("Whisper could not transcribe this recording.")?;
    let text = state
        .as_iter()
        .map(|segment| segment.to_string())
        .collect::<String>();
    Ok(text.trim().to_owned())
}

#[derive(Serialize)]
pub(crate) struct ModelStatus {
    #[serde(flatten)]
    model: &'static Model,
    installed: bool,
}

pub(crate) fn model_status(data_dir: &Path) -> Vec<ModelStatus> {
    MODELS
        .iter()
        .map(|model| ModelStatus {
            model,
            installed: installed(data_dir, model),
        })
        .collect()
}

fn lookup_model(id: &str) -> Result<&'static Model> {
    MODELS
        .iter()
        .find(|model| model.id == id)
        .context("Choose a downloadable Whisper model.")
}

fn model_path(data_dir: &Path, model: &Model) -> PathBuf {
    data_dir
        .join("speech-models")
        .join(format!("ggml-{}.bin", model.id))
}

fn installed(data_dir: &Path, model: &Model) -> bool {
    fs::metadata(model_path(data_dir, model))
        .is_ok_and(|file| file.is_file() && file.len() == model.bytes)
}

pub(crate) async fn download_model(data_dir: &Path, id: &str) -> Result<()> {
    let model = lookup_model(id)?;
    if installed(data_dir, model) {
        return Ok(());
    }
    let destination = model_path(data_dir, model);
    let parent = destination
        .parent()
        .context("Missing Whisper model directory.")?;
    tokio::fs::create_dir_all(parent).await?;
    let pending = tempfile::NamedTempFile::new_in(parent)?;
    let mut writer = tokio::fs::File::from_std(pending.reopen()?);
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .redirect(Policy::custom(|redirect| {
            if redirect.previous().len() > 5 {
                redirect.error("Whisper model download exceeded the redirect limit.")
            } else if !model_host(redirect.url()) {
                redirect.error("Whisper model download redirected to an unexpected host.")
            } else {
                redirect.follow()
            }
        }))
        .build()?;
    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/{MODEL_REVISION}/ggml-{}.bin",
        model.id
    );
    let mut response = client
        .get(url)
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .map_err(|error| error.without_url())?;
    ensure!(
        response.status().is_success(),
        "Whisper model download returned HTTP {}.",
        response.status()
    );
    if let Some(length) = response.content_length() {
        ensure!(
            length == model.bytes,
            "The Whisper model download has an unexpected size."
        );
    }
    let mut total = 0_u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url())?
    {
        total += chunk.len() as u64;
        ensure!(
            total <= model.bytes,
            "The Whisper model download has an unexpected size."
        );
        digest.update(&chunk);
        writer.write_all(&chunk).await?;
    }
    ensure!(
        total == model.bytes && format!("{:x}", digest.finalize()) == model.sha256,
        "Whisper model integrity verification failed."
    );
    writer.sync_all().await?;
    drop(writer);
    pending.persist(destination)?;
    Ok(())
}

fn model_host(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            matches!(
                host,
                "huggingface.co"
                    | "cas-bridge.xethub.hf.co"
                    | "transfer.xethub.hf.co"
                    | "transfer.xethub-eu.hf.co"
                    | "cdn-lfs.hf.co"
            ) || host.ends_with(".cdn.hf.co")
                || (host.starts_with("cdn-lfs-") && host.ends_with(".hf.co"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_redirects_require_a_trusted_https_host() {
        for url in [
            "https://huggingface.co/model",
            "https://us.aws.cdn.hf.co/model?signature=value",
            "https://us-east1.us.gcp.cdn.hf.co/model",
            "https://cas-bridge.xethub.hf.co/model",
            "https://transfer.xethub-eu.hf.co/model",
            "https://cdn-lfs-us-1.hf.co/model",
        ] {
            assert!(model_host(&Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "http://us.aws.cdn.hf.co/model",
            "https://us.aws.cdn.hf.co:8443/model",
            "https://user:secret@us.aws.cdn.hf.co/model",
            "https://us.aws.cdn.hf.co.example.org/model",
            "https://other-hf.co/model",
            "https://127.0.0.1/model",
        ] {
            assert!(!model_host(&Url::parse(url).unwrap()), "{url}");
        }
    }
}
