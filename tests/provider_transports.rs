use opendex::{
    codex::Bridge,
    images,
    mcp::Connection,
    providers::{self, Tool},
    state::{
        Attachment, LocalExecutionModel, Message, Provider, ProviderKind, Role, Server, Transport,
    },
    voice,
    web::{self, Action, Config},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread::JoinHandle,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Request {
    line: String,
    headers: String,
    body: Value,
}

fn read_request(stream: &TcpStream) -> Request {
    let mut read = BufReader::new(stream);
    let mut line = String::new();
    read.read_line(&mut line).unwrap();
    let mut headers = String::new();
    loop {
        let mut header = String::new();
        assert_ne!(read.read_line(&mut header).unwrap(), 0);
        if header == "\r\n" {
            break;
        }
        headers.push_str(&header.to_lowercase());
    }
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map_or(0, |length| length.parse::<usize>().unwrap());
    let mut body = vec![0; length];
    read.read_exact(&mut body).unwrap();
    let body = if length == 0 {
        Value::Null
    } else if headers.contains("application/x-www-form-urlencoded") {
        Value::String(String::from_utf8(body).unwrap())
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    Request {
        line,
        headers,
        body,
    }
}

struct Fixture {
    endpoint: String,
    worker: JoinHandle<()>,
}

impl Fixture {
    fn new(
        count: usize,
        mut serve: impl FnMut(Request) -> (&'static str, String, String) + Send + 'static,
    ) -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", socket.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for _ in 0..count {
                let (mut stream, _) = socket.accept().unwrap();
                let (mime, extra, body) = serve(read_request(&stream));
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n", body.len()).unwrap();
                let split = body.len() / 2;
                stream.write_all(&body.as_bytes()[..split]).unwrap();
                stream.flush().unwrap();
                stream.write_all(&body.as_bytes()[split..]).unwrap();
            }
        });
        Self { endpoint, worker }
    }

    fn finish(self) {
        self.worker.join().unwrap();
    }
}

#[tokio::test]
async fn voice_adapters_send_the_selected_speech_payload_and_decode_audio() {
    let wav = "RIFF0000WAVEfixture";
    let fixture = Fixture::new(5, move |request| {
        match request.line.split_whitespace().nth(1).unwrap() {
            "/custom/speech" => {
                assert!(request.headers.contains("x-audio-key: fixture"));
                assert_eq!(
                    request.body,
                    json!({
                        "model": "speech-model", "voice": "sample-voice", "input": "Hello & world!", "response_format": "mp3"
                    })
                );
                ("audio/mpeg", String::new(), "ID3fixture".into())
            }
            "/v1/text-to-speech/sample-voice?output_format=mp3_44100_128" => {
                assert!(request.headers.contains("xi-api-key: fixture"));
                assert_eq!(
                    request.body,
                    json!({"text": "Hello & world!", "model_id": "speech-model"})
                );
                ("audio/mpeg", String::new(), "ID3fixture".into())
            }
            "/v2/voices?page_size=100&search=calm" => {
                assert!(request.headers.contains("xi-api-key: fixture"));
                (
                    "application/json",
                    String::new(),
                    json!({"voices":[{"voice_id":"demo","name":"Calm"}]}).to_string(),
                )
            }
            "/tts" => {
                assert_eq!(
                    request.body,
                    "text=Hello+%26+world%21&voice_url=sample-voice"
                );
                ("audio/wav", String::new(), wav.into())
            }
            "/api/audio/speak" => {
                assert!(request.headers.contains("x-hermes-session-token: fixture"));
                assert_eq!(request.body, json!({"text": "Hello & world!"}));
                (
                    "application/json",
                    String::new(),
                    json!({
                        "data_url": "data:audio/wav;base64,UklGRjAwMDBXQVZFZml4dHVyZQ=="
                    })
                    .to_string(),
                )
            }
            url => panic!("Unexpected speech URL {url}"),
        }
    });
    let client = providers::client().unwrap();
    for (kind, path, header, mime) in [
        (
            voice::Kind::OpenAiCompatible,
            "/custom/speech",
            "x-audio-key",
            "audio/mpeg",
        ),
        (voice::Kind::ElevenLabs, "/", "xi-api-key", "audio/mpeg"),
        (voice::Kind::PocketTts, "/tts", "", "audio/wav"),
        (
            voice::Kind::Hermes,
            "/api/audio/speak",
            "x-hermes-session-token",
            "audio/wav",
        ),
    ] {
        let profile = voice::Profile {
            id: Uuid::new_v4(),
            name: "Voice fixture".into(),
            kind,
            endpoint: format!("{}{path}", fixture.endpoint),
            model: "speech-model".into(),
            voice: "sample-voice".into(),
            key_header: header.into(),
            has_secret: false,
            secret_ref: None,
        };
        let clip = voice::synthesize(&client, &profile, Some("fixture"), " Hello & world! ", 0)
            .await
            .unwrap();
        assert_eq!(clip.mime, mime);
        assert!(!clip.more);
        if kind == voice::Kind::ElevenLabs {
            let found = voice::elevenlabs_voices(&client, &profile, Some("fixture"), "calm")
                .await
                .unwrap();
            assert_eq!((&*found[0].id, &*found[0].name), ("demo", "Calm"));
        }
    }
    fixture.finish();
}

#[tokio::test]
async fn new_voice_connections_use_their_server_formats_and_playable_audio() {
    let mut deepgram_part = 0;
    let fixture = Fixture::new(7, move |request| {
        let path = request.line.split_whitespace().nth(1).unwrap();
        match path {
            "/" => assert_eq!(request.body, json!({"text": "Voice check."})),
            "/synthesize" => assert_eq!(
                request.body,
                json!({
                    "text": "Voice check.", "voice": "en_US-lessac-medium", "speaker_id": 2
                })
            ),
            "/omni/v1/audio/speech" => assert_eq!(
                request.body,
                json!({
                    "input": "Voice check.", "model": "omnivoice", "voice": "auto", "response_format": "wav"
                })
            ),
            "/kokoro/v1/audio/speech" => assert_eq!(
                request.body,
                json!({
                    "input": "Voice check.", "model": "kokoro", "voice": "af_heart", "response_format": "mp3"
                })
            ),
            "/tts/bytes" => {
                assert!(request.headers.contains("authorization: bearer fixture"));
                assert!(request.headers.contains("cartesia-version: 2026-08-14"));
                assert_eq!(
                    request.body,
                    json!({
                        "transcript": "Voice check.", "model_id": "sonic-3.6", "voice": "test-voice",
                        "output_format": {"container": "wav", "encoding": "pcm_s16le", "sample_rate": 24000}
                    })
                );
            }
            "/v1/speak?model=aura-2-thalia-en&encoding=linear16&container=wav" => {
                assert!(request.headers.contains("authorization: token fixture"));
                assert_eq!(
                    request.body["text"].as_str().unwrap().len(),
                    if deepgram_part == 0 { 2000 } else { 1 }
                );
                deepgram_part += 1;
            }
            _ => panic!("Unexpected speech path {path}"),
        }
        ("audio/wav", String::new(), "RIFF0000WAVEfixture".into())
    });
    let client = providers::client().unwrap();
    for (kind, path, model, speaker, key) in [
        (voice::Kind::Piper, "/", "", "", None),
        (
            voice::Kind::Piper,
            "/synthesize",
            "en_US-lessac-medium",
            "2",
            None,
        ),
        (
            voice::Kind::OmniVoice,
            "/omni/v1/audio/speech",
            "omnivoice",
            "auto",
            None,
        ),
        (
            voice::Kind::Kokoro,
            "/kokoro/v1/audio/speech",
            "kokoro",
            "af_heart",
            None,
        ),
        (
            voice::Kind::Cartesia,
            "/tts/bytes",
            "sonic-3.6",
            "test-voice",
            Some("fixture"),
        ),
        (
            voice::Kind::Deepgram,
            "/v1/speak",
            "aura-2-thalia-en",
            "",
            Some("fixture"),
        ),
    ] {
        let profile = voice::Profile {
            id: Uuid::new_v4(),
            name: "Voice fixture".into(),
            kind,
            endpoint: format!("{}{path}", fixture.endpoint),
            model: model.into(),
            voice: speaker.into(),
            key_header: String::new(),
            has_secret: key.is_some(),
            secret_ref: None,
        };
        if kind == voice::Kind::Deepgram {
            let text = "x".repeat(2001);
            assert!(
                voice::synthesize(&client, &profile, key, &text, 0)
                    .await
                    .unwrap()
                    .more
            );
            assert!(
                !voice::synthesize(&client, &profile, key, &text, 1)
                    .await
                    .unwrap()
                    .more
            );
        } else {
            let clip = voice::synthesize(&client, &profile, key, "Voice check.", 0)
                .await
                .unwrap();
            assert_eq!(clip.mime, "audio/wav");
            assert!(!clip.more);
        }
    }
    fixture.finish();
}

fn provider(fixture: &Fixture, kind: ProviderKind) -> Provider {
    Provider {
        kind,
        endpoint: format!("{}/api-prefix", fixture.endpoint),
        ..Provider::local()
    }
}

#[tokio::test]
async fn image_sources_submit_comfy_workflows_and_read_custom_json_images() {
    let fixture = Fixture::new(4, |request| {
        match request.line.split_whitespace().nth(1).unwrap() {
            "/comfy/prompt" => {
                assert_eq!(request.body["prompt"]["10"]["inputs"]["text"], "Coast: \"blue\"");
                assert_eq!(request.body["prompt"]["10"]["class_type"], "CLIPTextEncode");
                ("application/json", String::new(), json!({"prompt_id":"job-1"}).to_string())
            }
            "/comfy/history/job-1" => (
                "application/json", String::new(),
                json!({"job-1":{"outputs":{"11":{"images":[{"filename":"fixture.gif","type":"output"}]}}}}).to_string(),
            ),
            "/comfy/view?filename=fixture.gif&subfolder=&type=output" =>
                ("image/gif", String::new(), "GIF89afixture".into()),
            "/custom" => {
                assert_eq!(request.body, json!({"input":{"caption":"Coast: \"blue\""},"model":"fixture"}));
                ("application/json", String::new(), json!({"result":{"image":"/9j/Zml4dHVyZQ=="}}).to_string())
            }
            path => panic!("Unexpected image request for {path}"),
        }
    });
    let client = providers::client().unwrap();
    for (kind, route, options, pointer, mime) in [
        (
            "comfy_ui",
            "/comfy",
            json!({
                "10":{"class_type":"CLIPTextEncode","inputs":{"text":"{{prompt}}"}},
                "11":{"class_type":"SaveImage","inputs":{"images":["10",0]}}
            }),
            "",
            "image/gif",
        ),
        (
            "custom",
            "/custom",
            json!({"input":{"caption":"{{prompt}}"},"model":"fixture"}),
            "/result/image",
            "image/jpeg",
        ),
    ] {
        let mut source: images::Source = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "name": "Fixture", "kind": kind,
            "endpoint": format!("{}{route}", fixture.endpoint), "options": options, "output_pointer": pointer
        })).unwrap();
        source.validate().unwrap();
        let image = images::generate_source(&client, &source, "Coast: \"blue\"")
            .await
            .unwrap();
        assert_eq!(image.media_type.as_deref(), Some(mime));
    }
    fixture.finish();
}

#[tokio::test]
async fn comfy_library_inspects_nodes_and_saves_a_new_named_graph() {
    let fixture = Fixture::new(5, |request| {
        let target = request.line.split_whitespace().nth(1).unwrap();
        let body = match target {
            "/comfy/userdata?dir=workflows&recurse=true" => {
                json!(["nested/existing.json", "../outside.json", "notes.txt"])
            }
            "/comfy/userdata/workflows%2Fnested%2Fexisting.json" => json!({"nodes":[],"links":[]}),
            "/comfy/object_info/CLIPTextEncode" => json!({
                "CLIPTextEncode":{"input":{"required":{"text":["STRING"]}},"output":["CONDITIONING"]}
            }),
            "/comfy/object_info/SaveImage" => json!({
                "SaveImage":{"input":{"required":{"images":["IMAGE"]}},"output":[]}
            }),
            "/comfy/userdata/workflows%2FNew%20Graph.json?overwrite=false" => {
                assert!(request.line.starts_with("POST "));
                assert_eq!(request.body["1"]["inputs"]["text"], "{{prompt}}");
                json!("workflows/New Graph.json")
            }
            path => panic!("Unexpected ComfyUI path {path}"),
        };
        ("application/json", String::new(), body.to_string())
    });
    let mut source: images::Source = serde_json::from_value(json!({
        "id":Uuid::new_v4(), "name":"Comfy library", "kind":"comfy_ui",
        "endpoint":format!("{}/comfy", fixture.endpoint)
    }))
    .unwrap();
    source.validate().unwrap();
    assert!(!source.ready_to_generate());
    let client = providers::client().unwrap();
    let library = images::comfy::Library::new(&client, &source).unwrap();
    assert_eq!(library.workflows().await.unwrap(), ["nested/existing.json"]);
    let existing = library.read("nested/existing.json").await.unwrap();
    assert!(images::comfy::api_graph(existing).is_err());
    assert!(images::comfy::workflow_path("../outside.json").is_err());
    assert!(images::comfy::workflow_path("%2e%2e/outside.json").is_err());
    let graph = images::comfy::api_graph(json!({
        "1":{"class_type":"CLIPTextEncode","inputs":{"text":"{{prompt}}"}},
        "2":{"class_type":"SaveImage","inputs":{"images":["1",0]}}
    }))
    .unwrap();
    library.validate_new_graph(&graph).await.unwrap();
    let name = images::comfy::new_workflow_path("New Graph").unwrap();
    library.save_new(&name, &graph).await.unwrap();
    fixture.finish();
}

#[tokio::test]
async fn agent_gateway_requires_opt_in_and_approval_support_for_remote_hermes() {
    let fixture = Fixture::new(4, |request| {
        match request.line.split_whitespace().nth(1).unwrap() {
            "/api-prefix/capabilities" => (
                "application/json",
                String::new(),
                json!({
                    "object":"hermes.api_server.capabilities",
                    "platform":"hermes-agent",
                    "features": {"approval_events": true, "run_approval_response": true}
                })
                .to_string(),
            ),
            "/inference/capabilities" => (
                "application/json",
                String::new(),
                json!({"object":"inference.capabilities"}).to_string(),
            ),
            path => panic!("Unexpected request for {path}"),
        }
    });
    let client = providers::client().unwrap();
    let mut provider = provider(&fixture, ProviderKind::OpenAiCompatible);
    assert!(
        providers::ensure_client_tool_endpoint(&client, &provider, "hermes-agent")
            .await
            .unwrap_err()
            .to_string()
            .contains("Hermes Agent API")
    );
    assert!(
        providers::ensure_client_tool_endpoint(&client, &provider, "custom-alias")
            .await
            .unwrap_err()
            .to_string()
            .contains("Hermes Agent API")
    );
    provider.hermes_remote_approvals = true;
    providers::ensure_client_tool_endpoint(&client, &provider, "hermes-agent")
        .await
        .unwrap();
    provider.endpoint = format!("{}/inference", fixture.endpoint);
    assert!(
        providers::ensure_client_tool_endpoint(&client, &provider, "Hermes-4-70B")
            .await
            .is_err()
    );
    provider.hermes_remote_approvals = false;
    providers::ensure_client_tool_endpoint(&client, &provider, "Hermes-4-70B")
        .await
        .unwrap();
    fixture.finish();
}

#[test]
fn a_paired_model_is_used_for_local_execution_and_does_not_fall_back_to_the_vps() {
    let mut inference = Provider::local();
    inference.models.push("local-fixture".into());
    let agent = Provider {
        kind: ProviderKind::OpenAiCompatible,
        hermes_remote_approvals: true,
        local_execution: Some(LocalExecutionModel {
            provider: inference.id,
            model: "local-fixture".into(),
        }),
        ..Provider::local()
    };
    let providers = [inference];
    let (selected, model) = agent.execution_model("hermes-agent", &providers).unwrap();
    assert_eq!(selected.id, providers[0].id);
    assert_eq!(model, "local-fixture");
    assert!(agent.execution_model("hermes-agent", &[]).is_err());
}

#[tokio::test]
async fn vps_delegation_relays_exact_hermes_approval_decisions() {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let provider = Provider {
        kind: ProviderKind::OpenAiCompatible,
        endpoint: format!("http://{}/v1", socket.local_addr().unwrap()),
        hermes_remote_approvals: true,
        ..Provider::local()
    };
    let worker = std::thread::spawn(move || {
        let read = |expected: &str| {
            let (stream, _) = socket.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&stream);
            assert_eq!(request.line.trim_end(), expected);
            (stream, request)
        };
        let send_json = |mut stream: TcpStream, body: Value| {
            let body = body.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        };
        let (capabilities, _) = read("GET /v1/capabilities HTTP/1.1");
        send_json(
            capabilities,
            json!({"platform":"hermes-agent", "features":{
                "approval_events":true, "run_approval_response":true, "run_submission":true,
                "run_events_sse":true, "run_stop":true
            }}),
        );
        let (submission, request) = read("POST /v1/runs HTTP/1.1");
        assert!(request.headers.contains("idempotency-key: "));
        assert_eq!(request.body["input"], "Check the host status");
        assert_eq!(request.body["model"], "hermes-agent");
        assert_eq!(request.body.as_object().unwrap().len(), 3);
        send_json(submission, json!({"run_id":"run_fixture"}));
        let (mut stream, _) = read("GET /v1/runs/run_fixture/events HTTP/1.1");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        for (prompt_id, command, choice) in [("p1", "hostname", "once"), ("p2", "whoami", "deny")] {
            let event = json!({"event":"approval.request", "run_id":"run_fixture", "prompt_id":prompt_id,
                "execution_generation":4, "command":command,"choices":["once","deny"]});
            write!(stream, "event: approval.request\ndata: {event}\n\n").unwrap();
            let (decision, request) = read("POST /v1/runs/run_fixture/approval HTTP/1.1");
            assert_eq!(
                request.body,
                json!({"request_id":prompt_id,"execution_generation":4,"choice":choice})
            );
            send_json(decision, json!({"run_id":"run_fixture", "choice":choice}));
        }
        let completed =
            json!({"event":"run.completed","run_id":"run_fixture","output":"Fixture summary"});
        write!(stream, "data: {completed}\n\n").unwrap();
    });
    let client = providers::client().unwrap();
    let cancel = CancellationToken::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        providers::remote::delegate(
            &client,
            &provider,
            "hermes-agent",
            "Check the host status",
            &cancel,
            |approval| async move { approval["command"] == "hostname" },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome.as_deref(), Some("Fixture summary"));
    worker.join().unwrap();
}

fn response(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\r\n\r\n"))
        .collect()
}

fn tool() -> Tool {
    Tool {
        name: "read_file".into(),
        description: "Read an example file".into(),
        schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
    }
}

#[tokio::test]
async fn image_generation_preserves_raster_bytes_and_provider_options() {
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aGfkAAAAASUVORK5CYII=";
    let fixture = Fixture::new(3, |request| {
        assert_eq!(
            request.line.trim(),
            "POST /api-prefix/images/generations HTTP/1.1"
        );
        assert_eq!(request.body["prompt"], "A fixture image");
        assert_eq!(request.body["size"], "1024x1536");
        assert_eq!(request.body["quality"], "medium");
        let data = match request.body["model"].as_str().unwrap() {
            "image-api" => {
                assert!(request.headers.contains("x-opendex-test:"));
                assert!(request.body.get("response_format").is_none());
                json!({"b64_json":PNG})
            }
            "image-compatible" => {
                assert_eq!(request.body["response_format"], "b64_json");
                json!({"b64_json":PNG})
            }
            "image-remote-url" => json!({"url":"https://example.org/remote.png"}),
            model => panic!("unexpected image model {model}"),
        };
        (
            "application/json",
            String::new(),
            json!({"data":[data]}).to_string(),
        )
    });
    let client = providers::client().unwrap();
    let directory = tempfile::tempdir().unwrap();
    for (kind, selected) in [
        (ProviderKind::OpenAi, "image-api"),
        (ProviderKind::OpenAiCompatible, "image-compatible"),
        (ProviderKind::OpenAiCompatible, "image-remote-url"),
    ] {
        let mut connection = provider(&fixture, kind);
        connection.headers = vec![("x-opendex-test".into(), "PATH".into())];
        let mut config = images::Config {
            provider: connection.id,
            model: selected.into(),
            size: Some("1024X1536".into()),
            quality: Some("Medium".into()),
            request_base64: true,
        };
        config.validate().unwrap();
        let result = images::generate(&client, &connection, &config, "A fixture image").await;
        if selected == "image-remote-url" {
            assert!(result.unwrap_err().to_string().contains("another origin"));
            continue;
        }
        let result = result.unwrap();
        assert_eq!(result.media_type.as_deref(), Some("image/png"));
        assert_eq!(&*result.text, PNG);
        let path = directory.path().join(result.name.clone());
        images::save(&result, &path).unwrap();
        assert_eq!(
            fs::read(path).unwrap(),
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, PNG).unwrap()
        );
    }
    fixture.finish();
}

#[tokio::test]
async fn web_services_route_only_the_configured_queries_and_pages() {
    let fixture = Fixture::new(3, |request| {
        let body = match request.line.trim() {
            "GET /search?q=a%26b+c&format=json HTTP/1.1" => {
                json!({"results":[
                    {"url":"javascript:alert(1)","title":"Unsupported scheme"},
                    {"url":"https://example.org/guide#install","title":"Guide","content":"First snippet"},
                    {"url":"https://example.org/guide#install","title":"Duplicate"},
                    {"url":"https://example.org/news","title":"News"},
                    {"url":"https://example.org/other","title":"Beyond requested limit"}
                ]})
            }
            "POST /api-prefix/web_search HTTP/1.1" => {
                assert!(request.headers.contains("x-opendex-test:"));
                assert_eq!(request.body, json!({"query":"a&b c","max_results":2}));
                json!({"results":[{"url":"https://example.org/guide#install","title":"Guide","content":"Ollama snippet"}]})
            }
            "POST /api-prefix/web_fetch HTTP/1.1" => {
                assert!(request.headers.contains("x-opendex-test:"));
                assert_eq!(
                    request.body,
                    json!({"url":"https://example.org/guide#install"})
                );
                json!({"title":"Guide","content":"Page details","links":["/next"]})
            }
            other => panic!("unexpected web request {other}"),
        };
        ("application/json", String::new(), body.to_string())
    });
    let client = providers::client().unwrap();
    let query = || Action::parse(web::SEARCH, &json!({"query":"a&b c","max_results":2})).unwrap();
    let searxng = Config::Searxng {
        endpoint: format!("{}/search", fixture.endpoint),
    }
    .resolve([])
    .unwrap();
    assert_eq!(searxng.tools().len(), 1);
    let search = searxng.execute(&client, query()).await.unwrap();
    assert_eq!(search.query.as_deref(), Some("a&b c"));
    assert_eq!(search.sources.len(), 2);
    assert_eq!(search.sources[0].content, "First snippet");
    assert_eq!(search.sources[1].title, "News");

    let mut cloud = provider(&fixture, ProviderKind::OllamaCloud);
    cloud.headers = vec![("x-opendex-test".into(), "PATH".into())];
    let ollama = Config::Ollama { provider: cloud.id }
        .resolve([&cloud])
        .unwrap();
    assert_eq!(ollama.tools().len(), 2);
    let search = ollama.execute(&client, query()).await.unwrap();
    assert_eq!(search.sources[0].content, "Ollama snippet");
    let action = Action::parse(web::FETCH, &json!({"url":search.sources[0].url})).unwrap();
    let page = ollama.execute(&client, action).await.unwrap();
    assert_eq!(page.sources[0].url, "https://example.org/guide#install");
    assert_eq!(page.sources[0].content, "Page details");
    assert!(Action::parse(web::FETCH, &json!({"url":"file:///tmp/source"})).is_err());
    assert!(Action::parse(web::SEARCH, &json!({"query":"search","max_results":11})).is_err());
    fixture.finish();
}

#[tokio::test]
async fn mixed_attachments_reach_provider_wire_formats_in_order() {
    let fixture = Fixture::new(4, |request| {
        let (content, kinds, reply, mime) = match request.line.split_whitespace().nth(1).unwrap() {
            "/api-prefix/responses" => (
                &request.body["input"][0]["content"],
                ["input_text", "input_image", "input_file", "input_image"],
                response(&[
                    json!({"type":"response.output_text.delta","delta":"received"}),
                    json!({"type":"response.completed"}),
                ]),
                "text/event-stream",
            ),
            "/api-prefix/messages" => (
                &request.body["messages"][0]["content"],
                ["text", "image", "document", "image"],
                response(&[
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"received"}}),
                    json!({"type":"message_stop"}),
                ]),
                "text/event-stream",
            ),
            "/api-prefix/chat/completions" => (
                &request.body["messages"][0]["content"],
                ["text", "image_url", "file", "image_url"],
                response(&[
                    json!({"choices":[{"delta":{"content":"received"},"finish_reason":"stop"}]}),
                ]),
                "text/event-stream",
            ),
            "/api-prefix/chat" => {
                let user = &request.body["messages"][0];
                assert_eq!(user["images"], json!(["Zmlyc3Q=", "c2Vjb25k"]));
                assert!(
                    user["content"]
                        .as_str()
                        .unwrap()
                        .contains("Text attachment example")
                );
                return (
                    "application/x-ndjson",
                    String::new(),
                    json!({"message":{"content":"received"},"done":true}).to_string(),
                );
            }
            other => panic!("unexpected provider route {other}"),
        };
        let content = content.as_array().unwrap();
        assert_eq!(content.len(), 4);
        for (part, kind) in content.iter().zip(kinds) {
            assert_eq!(part["type"], kind);
        }
        assert!(
            content[0]["text"]
                .as_str()
                .unwrap()
                .contains("Text attachment example")
        );
        for (part, expected) in content[1..]
            .iter()
            .zip(["Zmlyc3Q=", "JVBERg==", "c2Vjb25k"])
        {
            assert!(part.to_string().contains(expected));
        }
        (mime, String::new(), reply)
    });
    let client = providers::client().unwrap();
    for kind in [
        ProviderKind::OpenAi,
        ProviderKind::Anthropic,
        ProviderKind::OpenAiCompatible,
        ProviderKind::Ollama,
    ] {
        let mut message = Message::new(Role::User, "Check these attachments.");
        message.attachments = [
            ("notes.txt", "Text attachment example", None),
            ("first.png", "Zmlyc3Q=", Some("image/png")),
            ("handbook.pdf", "JVBERg==", Some("application/pdf")),
            ("second.jpg", "c2Vjb25k", Some("image/jpeg")),
        ]
        .into_iter()
        .filter(|(_, _, mime)| kind != ProviderKind::Ollama || *mime != Some("application/pdf"))
        .map(|(name, text, media_type)| Attachment {
            name: name.into(),
            text: text.into(),
            media_type: media_type.map(str::to_owned),
        })
        .collect();
        let mut connection = provider(&fixture, kind);
        connection.headers = vec![("x-opendex-test".into(), "PATH".into())];
        let result =
            providers::generate(&client, &connection, "fixture", "", &[message], &[], |_| {})
                .await
                .unwrap();
        assert_eq!(result.text, "received");
    }
    fixture.finish();
}

#[tokio::test]
async fn openai_compatible_streams_parallel_tool_fragments_and_round_trips_results() {
    let fixture = Fixture::new(3, |request| {
        if request.line.starts_with("GET") {
            assert_eq!(request.line.trim(), "GET /api-prefix/models HTTP/1.1");
            return (
                "application/json",
                String::new(),
                json!({"data":[{"id":"test-model"},{"id":"test-model"}]}).to_string(),
            );
        }
        assert_eq!(
            request.line.trim(),
            "POST /api-prefix/chat/completions HTTP/1.1"
        );
        assert_eq!(request.body["messages"][0]["role"], "system");
        if request.body["messages"].as_array().unwrap().len() > 2 {
            assert_eq!(
                request.body["messages"][2]["tool_calls"][0]["id"],
                "call_123"
            );
            assert_eq!(request.body["messages"][3]["tool_call_id"], "call_123");
            return (
                "text/event-stream",
                String::new(),
                response(&[
                    json!({"choices":[{"delta":{"content":"Résumé"},"finish_reason":null}]}),
                    json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
                ]),
            );
        }
        assert_eq!(request.body["tools"][0]["function"]["name"], "read_file");
        (
            "text/event-stream",
            String::new(),
            response(&[
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_123","function":{"name":"read_file","arguments":"{\"pa"}}]},"finish_reason":null}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"README.md\"}"}}]},"finish_reason":null}]}),
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ]),
        )
    });
    let client = providers::client().unwrap();
    let provider = provider(&fixture, ProviderKind::OpenAiCompatible);
    assert_eq!(
        providers::discover(&client, &provider).await.unwrap(),
        ["test-model"]
    );
    let mut messages = vec![Message::new(Role::User, "Read the file")];
    let completion = providers::generate(
        &client,
        &provider,
        "test-model",
        "test context",
        &messages,
        &[tool()],
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(completion.calls[0].arguments, json!({"path":"README.md"}));
    let mut called = Message::new(Role::Assistant, "");
    called.calls = completion.calls;
    messages.push(called);
    let mut result = Message::new(Role::Tool, "example text");
    result.tool_id = Some("call_123".into());
    messages.push(result);
    let mut streamed = String::new();
    let done = providers::generate(
        &client,
        &provider,
        "test-model",
        "test context",
        &messages,
        &[],
        |delta| streamed.push_str(delta),
    )
    .await
    .unwrap();
    assert_eq!(streamed, "Résumé");
    assert_eq!(done.text, streamed);
    fixture.finish();
}

#[tokio::test]
async fn anthropic_and_responses_send_native_payloads_and_decode_events() {
    let fixture = Fixture::new(2, |request| {
        if request.line.contains("/messages") {
            assert!(request.headers.contains("anthropic-version: 2023-06-01"));
            assert_eq!(request.body["messages"][0]["content"][0]["text"], "Hello");
            assert_eq!(request.body["system"], "A system instruction");
            return (
                "text/event-stream",
                String::new(),
                response(&[
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Claude fixture"}}),
                    json!({"type":"message_stop"}),
                ]),
            );
        }
        assert_eq!(request.line.trim(), "POST /api-prefix/responses HTTP/1.1");
        assert_eq!(request.body["input"][0]["content"], "Hello");
        assert_eq!(request.body["store"], false);
        (
            "text/event-stream",
            String::new(),
            response(&[
                json!({"type":"response.output_text.delta","delta":"OpenAI fixture"}),
                json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","name":"read_file","call_id":"call_sample","arguments":"{\"path\":\"README.md\"}"}}),
                json!({"type":"response.completed"}),
            ]),
        )
    });
    let client = providers::client().unwrap();
    let messages = vec![Message::new(Role::User, "Hello")];
    for (kind, expected) in [
        (ProviderKind::Anthropic, "Claude fixture"),
        (ProviderKind::OpenAi, "OpenAI fixture"),
    ] {
        let mut provider = provider(&fixture, kind);
        provider.headers = vec![("x-opendex-test".into(), "PATH".into())];
        let completion = providers::generate(
            &client,
            &provider,
            "fixture",
            "A system instruction",
            &messages,
            &[tool()],
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(completion.text, expected);
        if kind == ProviderKind::OpenAi {
            assert_eq!(completion.calls[0].id, "call_sample");
        }
    }
    fixture.finish();
}

#[tokio::test]
async fn ollama_ndjson_stops_on_done_and_retains_tool_arguments() {
    let fixture = Fixture::new(1, |request| {
        assert_eq!(request.line.trim(), "POST /api-prefix/chat HTTP/1.1");
        assert_eq!(request.body["messages"][1]["content"], "Hello");
        let body = [
            json!({"message":{"content":"Ollama fixture"},"done":false}),
            json!({"message":{"content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"README.md"}}}]},"done":false}),
            json!({"message":{"content":""},"done":true}),
        ].map(|row| row.to_string()).join("\n");
        ("application/x-ndjson", String::new(), body)
    });
    let completion = providers::generate(
        &providers::client().unwrap(),
        &provider(&fixture, ProviderKind::Ollama),
        "fixture",
        "System",
        &[Message::new(Role::User, "Hello")],
        &[tool()],
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(completion.text, "Ollama fixture");
    assert_eq!(completion.calls[0].arguments["path"], "README.md");
    fixture.finish();
}

fn server(address: String, current_protocol: bool) -> Server {
    Server {
        id: Uuid::new_v4(),
        name: "Fixture MCP".into(),
        transport: Transport::Http,
        address,
        args: vec![],
        env: vec![],
        has_secret: false,
        secret_ref: None,
        enabled: true,
        blocked_tools: vec![],
        tool_permission: None,
        tool_permissions: Default::default(),
        current_protocol,
        headers: Vec::new(),
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_mcp_stdio_sends_fixed_and_passed_environment() {
    let script = r#"
import json, os, sys
assert os.environ['OPENDEX_MCP_FIXED'] == 'fixture-value'
assert os.pathsep in os.environ['PATH']
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    if request['method'] == 'initialize':
        result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}, "serverInfo": {"name": "env", "version": "1"}}
    elif request['method'] == 'tools/list':
        result = {"tools": [{"name": "echo", "description": "Reads its environment", "inputSchema": {"type": "object"}}]}
    elif request['method'] == 'notifications/initialized':
        continue
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request['id'], "result": result}), flush=True)
"#;
    let mut local = server("python3".into(), false);
    local.transport = Transport::Stdio;
    local.args = vec!["-u".into(), "-c".into(), script.into()];
    local.env = vec!["OPENDEX_MCP_FIXED=fixture-value".into(), "PATH".into()];
    local.validate().unwrap();
    let mut session = Connection::connect(&providers::client().unwrap(), &local, None)
        .await
        .unwrap();
    assert_eq!(session.tools().await.unwrap()[0].name, "echo");
    session.close().await;
}

#[tokio::test]
async fn legacy_mcp_http_keeps_session_between_initialize_list_call_and_delete() {
    let fixture = Fixture::new(5, |request| {
        let method = request.body["method"].as_str().unwrap_or("");
        let extra = if method == "initialize" {
            "Mcp-Session-Id: fixture-session\r\n"
        } else {
            assert!(request.headers.contains("mcp-session-id: fixture-session"));
            ""
        };
        let result = match method {
            "initialize" => {
                json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"echo","description":"Echo fixture","inputSchema":{"type":"object"}}]})
            }
            "tools/call" => json!({"content":[{"type":"text","text":"MCP fixture output"}]}),
            "notifications/initialized" | "" => json!({}),
            _ => panic!("unexpected MCP method {method}"),
        };
        let value = json!({"jsonrpc":"2.0","id":request.body["id"],"result":result});
        if method == "tools/call" {
            ("text/event-stream", extra.into(), response(&[value]))
        } else {
            ("application/json", extra.into(), value.to_string())
        }
    });
    let mut session = Connection::connect(
        &providers::client().unwrap(),
        &server(fixture.endpoint.clone(), false),
        None,
    )
    .await
    .unwrap();
    assert_eq!(session.tools().await.unwrap()[0].name, "echo");
    assert_eq!(
        session.call("echo", json!({"key":"value"})).await.unwrap(),
        "MCP fixture output"
    );
    session.close().await;
    fixture.finish();
}

#[tokio::test]
async fn current_mcp_sets_protocol_and_routing_headers_without_handshake() {
    let fixture = Fixture::new(2, |request| {
        let method = request.body["method"].as_str().unwrap();
        assert!(request.headers.contains(&format!("mcp-method: {method}")));
        assert!(request.headers.contains("mcp-protocol-version: 2026-07-28"));
        assert_eq!(
            request.body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
            "2026-07-28"
        );
        let result = if method == "tools/list" {
            json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
        } else {
            assert_eq!(method, "tools/call");
            assert!(request.headers.contains("mcp-name: echo"));
            json!({"structuredContent":{"ok":true}})
        };
        (
            "application/json",
            String::new(),
            json!({"jsonrpc":"2.0","id":request.body["id"],"result":result}).to_string(),
        )
    });
    let mut session = Connection::connect(
        &providers::client().unwrap(),
        &server(fixture.endpoint.clone(), true),
        None,
    )
    .await
    .unwrap();
    assert_eq!(session.tools().await.unwrap()[0].name, "echo");
    assert_eq!(
        session.call("echo", json!({})).await.unwrap(),
        "{\"ok\":true}"
    );
    session.close().await;
    fixture.finish();
}

#[tokio::test]
#[ignore = "requires a locally installed Codex CLI"]
async fn installed_codex_server_initializes_with_isolated_account_state() {
    let folder = tempfile::tempdir().unwrap();
    let program = std::env::var("OPENDEX_TEST_CODEX").unwrap_or_else(|_| "codex".into());
    let home = PathBuf::from(folder.path()).join("codex");
    let mut bridge = Bridge::start(&program, &home).await.unwrap();
    assert!(!bridge.account().await.unwrap().connected);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_mcp_stdio_exchanges_newline_delimited_json_rpc() {
    let script = r#"
import json
import sys
for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if "id" not in request:
        continue
    if method == "initialize":
        result = {"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}
    else:
        assert request["params"]["name"] == "echo"
        result = {"content":[{"type":"text","text":request["params"]["arguments"]["word"]}]}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
"#;
    let mut local = server("python3".into(), false);
    local.transport = Transport::Stdio;
    local.args = vec!["-u".into(), "-c".into(), script.into()];
    let mut session = Connection::connect(&providers::client().unwrap(), &local, None)
        .await
        .unwrap();
    assert_eq!(session.tools().await.unwrap()[0].name, "echo");
    assert_eq!(
        session
            .call("echo", json!({"word":"stdio verified"}))
            .await
            .unwrap(),
        "stdio verified"
    );
    session.close().await;
}

#[tokio::test]
async fn current_mcp_lists_pages_reads_resources_and_renders_prompt_arguments() {
    let fixture = Fixture::new(7, |request| {
        let method = request.body["method"].as_str().unwrap();
        assert!(request.headers.contains(&format!("mcp-method: {method}")));
        assert!(request.headers.contains("mcp-protocol-version: 2026-07-28"));
        let args = &request.body["params"];
        let result = match method {
            "server/discover" => json!({"capabilities":{"resources":{},"prompts":{}}}),
            "resources/list" if args["cursor"] == "page-2" => {
                json!({"resources":[{"uri":"sample://notes/second","name":"Second note"}]})
            }
            "resources/list" => {
                assert!(args["cursor"].is_null());
                json!({"resources":[{"uri":"sample://notes/first","name":"First note"}],"nextCursor":"page-2"})
            }
            "resources/templates/list" => {
                json!({"resourceTemplates":[{"uriTemplate":"sample://notes/{name}","name":"Find note"}]})
            }
            "prompts/list" => json!({"prompts":[{"name":"review","title":"Review text",
                "arguments":[{"name":"topic","required":true}]}]}),
            "resources/read" => {
                assert_eq!(args["uri"], "sample://notes/second");
                assert!(request.headers.contains("mcp-name: sample://notes/second"));
                json!({"contents":[{"uri":args["uri"],"text":"Fieldnotes from MCP."},
                                   {"uri":"sample://notes/image","blob":"AAAA"}]})
            }
            "prompts/get" => {
                assert_eq!(args["name"], "review");
                assert_eq!(args["arguments"], json!({"topic":"Release notes"}));
                assert!(request.headers.contains("mcp-name: review"));
                json!({"messages":[
                    {"role":"assistant","content":{"type":"text","text":"I can review a draft."}},
                    {"role":"user","content":{"type":"resource","resource":{"uri":"sample://draft","text":"Review Release notes."}}}
                ]})
            }
            _ => panic!("unexpected MCP method {method}"),
        };
        let response = json!({"jsonrpc":"2.0","id":request.body["id"],"result":result});
        ("application/json", String::new(), response.to_string())
    });
    let mut session = Connection::connect(
        &providers::client().unwrap(),
        &server(fixture.endpoint.clone(), true),
        None,
    )
    .await
    .unwrap();
    let catalog = session.context_catalog().await.unwrap();
    assert_eq!(catalog.resources.len(), 2);
    assert_eq!(catalog.templates[0].uri_template, "sample://notes/{name}");
    assert!(catalog.prompts[0].arguments[0].required);
    let resource = session
        .read_resource(&catalog.resources[1].uri)
        .await
        .unwrap();
    assert_eq!(resource.binary_count, 1);
    assert!(resource.text.contains("Fieldnotes from MCP."));
    let messages = session
        .get_prompt("review", [("topic".into(), "Release notes".into())].into())
        .await
        .unwrap();
    assert_eq!(messages[0].role, "assistant");
    assert_eq!(messages[1].text, "Review Release notes.");
    session.close().await;
    fixture.finish();
}

#[tokio::test]
async fn legacy_mcp_catalog_preserves_resources_when_templates_are_unavailable() {
    let fixture = Fixture::new(4, |request| {
        let method = request.body["method"].as_str().unwrap();
        let result = match method {
            "initialize" => json!({"protocolVersion":"2025-11-25","capabilities":{"resources":{}}}),
            "notifications/initialized" => json!({}),
            "resources/list" => json!({"resources":[{"uri":"sample://first","name":"First"}]}),
            "resources/templates/list" => {
                let error = json!({"jsonrpc":"2.0","id":request.body["id"],"error":{"code":-32601,"message":"Method unavailable"}});
                return ("application/json", String::new(), error.to_string());
            }
            _ => panic!("unexpected MCP method {method}"),
        };
        let response = json!({"jsonrpc":"2.0","id":request.body["id"],"result":result});
        ("application/json", String::new(), response.to_string())
    });
    let mut session = Connection::connect(
        &providers::client().unwrap(),
        &server(fixture.endpoint.clone(), false),
        None,
    )
    .await
    .unwrap();
    let catalog = session.context_catalog().await.unwrap();
    assert_eq!(catalog.resources[0].name, "First");
    assert!(catalog.templates.is_empty());
    assert!(catalog.prompts.is_empty());
    session.close().await;
    fixture.finish();
}

#[tokio::test]
async fn a_resource_only_message_reaches_chat_completions() {
    let fixture = Fixture::new(1, |request| {
        let message = &request.body["messages"][0];
        assert_eq!(message["role"], "user");
        let content = message["content"].as_str().unwrap();
        assert!(content.contains("sample://notes/second"));
        assert!(content.contains("Fieldnotes from MCP."));
        (
            "text/event-stream",
            String::new(),
            response(&[
                json!({"choices":[{"delta":{"content":"Received resource"},"finish_reason":"stop"}]}),
            ]),
        )
    });
    let mut message = Message::new(Role::User, "");
    message.resources.push(opendex::state::McpResource {
        server_id: Uuid::new_v4(),
        source: "Notes".into(),
        name: "Second note".into(),
        uri: "sample://notes/second".into(),
        text: "Fieldnotes from MCP.".into(),
    });
    assert!(message.has_content());
    let result = providers::generate(
        &providers::client().unwrap(),
        &provider(&fixture, ProviderKind::OpenAiCompatible),
        "fixture",
        "",
        &[message],
        &[],
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.text, "Received resource");
    fixture.finish();
}

#[tokio::test]
async fn native_gemini_discovers_chat_models_across_pages() {
    let fixture = Fixture::new(2, |request| {
        assert!(request.headers.contains("x-gemini-fixture:"));
        let body = match request.line.trim() {
            "GET /api-prefix/models HTTP/1.1" => json!({
                "models": [
                    {"name":"models/gemini-fixture-flash","supportedGenerationMethods":["generateContent","countTokens"]},
                    {"name":"models/gemini-fixture-embed","supportedGenerationMethods":["embedContent"]}
                ],
                "nextPageToken":"page/2"
            }),
            "GET /api-prefix/models?pageToken=page%2F2 HTTP/1.1" => json!({
                "models": [
                    {"name":"models/gemini-fixture-pro","supportedGenerationMethods":["generateContent"]},
                    {"name":"models/gemini-fixture-flash","supportedGenerationMethods":["generateContent"]}
                ]
            }),
            path => panic!("unexpected catalog route {path}"),
        };
        ("application/json", String::new(), body.to_string())
    });
    let mut connection = provider(&fixture, ProviderKind::Gemini);
    connection.headers = vec![("x-gemini-fixture".into(), "PATH".into())];
    let models = providers::discover(&providers::client().unwrap(), &connection)
        .await
        .unwrap();
    assert_eq!(models, ["gemini-fixture-flash", "gemini-fixture-pro"]);
    fixture.finish();
}

#[tokio::test]
async fn native_gemini_replays_signed_parallel_and_sequential_calls() {
    let first_call = json!({"functionCall":{"id":"native-one","name":"read_file","args":{"path":"first.txt"}},
                           "thoughtSignature":"c2lnbmF0dXJlLW9uZQ=="});
    let second_call = json!({"functionCall":{"name":"read_file","args":{"path":"second.txt"}}});
    let third_call = json!({"functionCall":{"name":"read_file","args":{"path":"third.txt"}},
                           "thoughtSignature":"c2lnbmF0dXJlLXRocmVl"});
    let model_parts = [first_call, second_call, third_call];
    let server_parts = model_parts.clone();

    let fixture = Fixture::new(3, move |request| {
        assert_eq!(
            request.line.trim(),
            "POST /api-prefix/models/gemini-fixture:streamGenerateContent?alt=sse HTTP/1.1"
        );
        assert!(request.headers.contains("x-gemini-fixture:"));
        assert_eq!(
            request.body["systemInstruction"]["parts"][0]["text"],
            "Use the selected workspace."
        );
        assert_eq!(request.body["generationConfig"]["maxOutputTokens"], 2048);
        assert_eq!(
            request.body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
            "object"
        );
        let contents = request.body["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
        assert!(
            contents[0]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Compare these files.")
        );
        assert_eq!(
            contents[0]["parts"][1]["inlineData"]["mimeType"],
            "application/pdf"
        );
        assert_eq!(contents[0]["parts"][1]["inlineData"]["data"], "JVBERg==");
        assert_eq!(
            contents[0]["parts"][2]["inlineData"]["mimeType"],
            "image/png"
        );
        assert_eq!(contents[0]["parts"][2]["inlineData"]["data"], "aW1hZ2U=");
        let candidate = |parts: Value| json!({"candidates":[{"content":{"role":"model","parts":parts},"index":0}]});
        let finish = json!({"candidates":[{"index":0,"finishReason":"STOP"}]});
        let chunks = match contents.len() {
            1 => vec![
                candidate(json!([{"text":"Thought summary","thought":true}, server_parts[0]])),
                candidate(json!([server_parts[1]])),
                finish,
            ],
            3 | 5 => {
                assert_eq!(contents[1]["parts"][1], server_parts[0]);
                assert_eq!(contents[1]["parts"][2], server_parts[1]);
                assert_eq!(contents[2]["role"], "user");
                let results = contents[2]["parts"].as_array().unwrap();
                assert_eq!(results.len(), 2);
                assert_eq!(results[0]["functionResponse"]["id"], "native-one");
                assert!(results[1]["functionResponse"].get("id").is_none());
                assert_eq!(
                    results[1]["functionResponse"]["response"]["output"],
                    "second output"
                );
                if contents.len() == 3 {
                    vec![candidate(json!([server_parts[2]])), finish]
                } else {
                    assert_eq!(contents[3]["parts"][0], server_parts[2]);
                    assert_eq!(
                        contents[4]["parts"][0]["functionResponse"]["response"]["output"],
                        "third output"
                    );
                    vec![
                        candidate(json!([{"text":"Checked "}])),
                        candidate(json!([{"text":"three files."}])),
                        finish,
                    ]
                }
            }
            size => panic!("unexpected Gemini history length {size}"),
        };
        ("text/event-stream", String::new(), response(&chunks))
    });

    let mut connection = provider(&fixture, ProviderKind::Gemini);
    connection.headers = vec![("x-gemini-fixture".into(), "PATH".into())];
    connection.token_limit = Some(2048);
    let client = providers::client().unwrap();
    let spec = [tool()];
    let mut user = Message::new(Role::User, "Compare these files.");
    user.attachments.push(Attachment {
        name: "notes.pdf".into(),
        text: "JVBERg==".into(),
        media_type: Some("application/pdf".into()),
    });
    user.attachments.push(Attachment {
        name: "chart.png".into(),
        text: "aW1hZ2U=".into(),
        media_type: Some("image/png".into()),
    });
    let mut history = vec![user];
    for outputs in [
        &["first output", "second output"][..],
        &["third output"][..],
    ] {
        let completion = providers::generate(
            &client,
            &connection,
            "models/gemini-fixture",
            "Use the selected workspace.",
            &history,
            &spec,
            |_| panic!("unexpected text delta"),
        )
        .await
        .unwrap();
        assert_eq!(completion.calls.len(), outputs.len());
        let mut model = Message::new(Role::Assistant, completion.text);
        model.calls = completion.calls.clone();
        model.output_items = completion.output_items;
        history.push(model);
        for (call, output) in completion.calls.into_iter().zip(outputs) {
            let mut result = Message::new(Role::Tool, *output);
            result.tool_id = Some(call.id);
            result.tool_name = Some(call.name);
            history.push(result);
        }
    }

    let mut streamed = String::new();
    let final_reply = providers::generate(
        &client,
        &connection,
        "gemini-fixture",
        "Use the selected workspace.",
        &history,
        &spec,
        |text| streamed.push_str(text),
    )
    .await
    .unwrap();
    assert_eq!(streamed, "Checked three files.");
    assert_eq!(final_reply.text, streamed);
    assert!(final_reply.calls.is_empty());
    fixture.finish();
}

#[tokio::test]
async fn native_gemini_requires_a_completed_tool_stream() {
    let fixture = Fixture::new(1, |_| {
        let incomplete = json!({"candidates":[{"content":{"parts":[
            {"functionCall":{"name":"read_file","args":{"path":"sample.txt"}}}
        ]}}]});
        ("text/event-stream", String::new(), response(&[incomplete]))
    });
    let mut connection = provider(&fixture, ProviderKind::Gemini);
    connection.headers = vec![("x-gemini-fixture".into(), "PATH".into())];
    let error = providers::generate(
        &providers::client().unwrap(),
        &connection,
        "gemini-fixture",
        "",
        &[Message::new(Role::User, "Read the sample.")],
        &[tool()],
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("before reporting completion"));
    fixture.finish();
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires installed Codex CLI and native Keychain access for disposable credentials"]
async fn installed_codex_keyring_format_matches_portable_entry() {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use zeroize::Zeroizing;
    struct Cleanup(Vec<keyring::Entry>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for entry in &self.0 {
                let _ = entry.delete_credential();
            }
        }
    }
    let folder = tempfile::tempdir().unwrap();
    let program = std::env::var("OPENDEX_TEST_CODEX").unwrap_or_else(|_| "codex".into());
    let source = folder.path().join("source");
    let destination = folder.path().join("destination");
    let entries = Cleanup(vec![
        opendex::codex::credential_entry(&source).unwrap(),
        opendex::codex::credential_entry(&destination).unwrap(),
    ]);
    let mut child = tokio::process::Command::new(&program)
        .env("CODEX_HOME", &source)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .args([
            "-c",
            "cli_auth_credentials_store=\"keyring\"",
            "-c",
            "auth_keyring_backend=\"direct\"",
            "login",
            "--with-api-key",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"sk-opendex-disposable-format-fixture\n")
        .await
        .unwrap();
    drop(stdin);
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "Codex could not create the disposable keyring entry"
    );
    let raw = Zeroizing::new(entries.0[0].get_password().unwrap());
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert!(value["OPENAI_API_KEY"].as_str() == Some("sk-opendex-disposable-format-fixture"));
    entries.0[1].set_password(&raw).unwrap();
    let mut check = tokio::process::Command::new(&program)
        .env("CODEX_HOME", &destination)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .args([
            "-c",
            "cli_auth_credentials_store=\"keyring\"",
            "-c",
            "auth_keyring_backend=\"direct\"",
            "login",
            "status",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(20), check.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "Codex did not recognize the destination keyring entry"
    );
    for entry in &entries.0 {
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
    }
}

#[tokio::test]
async fn current_mcp_http_servers_send_environment_backed_headers() {
    let fixture = Fixture::new(1, |request| {
        assert!(request.headers.contains("x-mcp-org:"));
        (
            "application/json",
            String::new(),
            json!({"jsonrpc":"2.0","id":request.body["id"],"result":{"tools":[]}}).to_string(),
        )
    });
    let mut connection = server(fixture.endpoint.clone(), true);
    connection.headers = vec![("x-mcp-org".into(), "PATH".into())];
    let mut session = Connection::connect(&providers::client().unwrap(), &connection, None)
        .await
        .unwrap();
    assert!(session.tools().await.unwrap().is_empty());
    session.close().await;
    fixture.finish();
}

#[test]
fn ollama_search_parses_only_library_result_cards() {
    let html = r#"
<a href="/search">Models</a>
<a href="/library/gemma">First</a>
<a href="/library/dolphin-phi">Second</a>
<a href="/library/gemma">Duplicate</a>
<a href="/library/bad model/rejected">Injected</a>
<a href="/library/%2Fescape">Escaped</a>
"#;
    assert_eq!(
        providers::parse_ollama_search(html),
        ["dolphin-phi", "gemma"]
    );
}
