use opendex::{
    codex::Bridge,
    mcp::Connection,
    providers::{self, Tool},
    state::{Message, Provider, ProviderKind, Role, Server, Transport},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    thread::JoinHandle,
};
use uuid::Uuid;

struct Request {
    line: String,
    headers: String,
    body: Value,
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
                let mut read = BufReader::new(&stream);
                let mut line = String::new();
                read.read_line(&mut line).unwrap();
                let mut headers = String::new();
                loop {
                    let mut header = String::new();
                    read.read_line(&mut header).unwrap();
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
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                let (mime, extra, body) = serve(Request {
                    line,
                    headers,
                    body,
                });
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

fn provider(fixture: &Fixture, kind: ProviderKind) -> Provider {
    Provider {
        kind,
        endpoint: format!("{}/api-prefix", fixture.endpoint),
        ..Provider::local()
    }
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
        enabled: true,
        blocked_tools: vec![],
        current_protocol,
    }
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
    user.attachment = Some(opendex::state::Attachment {
        name: "notes.pdf".into(),
        text: "JVBERg==".into(),
        media_type: Some("application/pdf".into()),
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
