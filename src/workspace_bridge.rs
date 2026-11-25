//! Streamable HTTP MCP tools gated by a live workspace grant.
use crate::{
    agent,
    state::{Mode, Permissions, ToolCall},
};
use anyhow::Result;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode, body::Bytes, body::Incoming, header,
    server::conn::http1, service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const LEGACY: &str = "2025-11-25";
const CURRENT: &str = "2026-07-28";
const REQUEST_LIMIT: usize = 128 * 1024;

pub struct Bridge {
    pub listen: SocketAddr,
    state: Arc<Shared>,
    stop: CancellationToken,
}

struct Shared {
    active: Mutex<HashMap<Uuid, (Access, mpsc::Sender<Invocation>)>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Turn,
    Link,
}

pub struct Invocation {
    pub call: ToolCall,
    pub reply: oneshot::Sender<(bool, String)>,
}

pub struct Lease {
    pub ticket: Uuid,
    pub stop: CancellationToken,
    state: Arc<Shared>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.state
            .active
            .lock()
            .expect("bridge lock")
            .remove(&self.ticket);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.cancel();
        self.state.active.lock().expect("bridge lock").clear();
    }
}

impl Bridge {
    pub async fn start(listen: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(listen).await?;
        let listen = listener.local_addr()?;
        let state = Arc::new(Shared {
            active: Mutex::new(HashMap::new()),
        });
        let stop = CancellationToken::new();
        tokio::spawn(serve(listener, Arc::clone(&state), stop.clone()));
        Ok(Self {
            listen,
            state,
            stop,
        })
    }

    pub fn register(&self, access: Access) -> (Lease, mpsc::Receiver<Invocation>) {
        let ticket = Uuid::new_v4();
        let (send, receive) = mpsc::channel(1);
        self.state
            .active
            .lock()
            .expect("bridge lock")
            .insert(ticket, (access, send));
        (
            Lease {
                ticket,
                stop: self.stop.child_token(),
                state: Arc::clone(&self.state),
            },
            receive,
        )
    }
}

async fn serve(listener: TcpListener, state: Arc<Shared>, stop: CancellationToken) {
    let connections = Arc::new(Semaphore::new(16));
    while let Some(Ok((socket, _))) = stop.run_until_cancelled(listener.accept()).await {
        let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
            continue;
        };
        let state = Arc::clone(&state);
        let stop = stop.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let connection = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10))
                .keep_alive(false)
                .serve_connection(
                    TokioIo::new(socket),
                    service_fn(move |request| {
                        let state = Arc::clone(&state);
                        async move { Ok::<_, Infallible>(handle(request, state).await) }
                    }),
                );
            let _ = stop.run_until_cancelled(connection).await;
        });
    }
}

type HttpReply = Response<Full<Bytes>>;

fn respond(status: StatusCode, body: Option<Value>) -> HttpReply {
    let mut response = Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store");
    let body = if let Some(body) = body {
        response = response.header(header::CONTENT_TYPE, "application/json");
        Bytes::from(body.to_string())
    } else {
        Bytes::new()
    };
    response
        .body(Full::new(body))
        .expect("static HTTP response headers")
}

fn tool_definitions(linked: bool) -> Vec<Value> {
    agent::tools(Mode::Agent, Some(Path::new(".")), &Permissions::default())
        .into_iter()
        .map(|mut tool| {
            if !linked {
                tool.schema["properties"]["workspace_session"] = json!({
                    "type": "string",
                    "description": "The workspace grant issued by OpenDex in the current turn instructions."
                });
                tool.schema["required"].as_array_mut().expect("agent tool schema")
                    .push(json!("workspace_session"));
            }
            json!({"name":tool.name, "description":format!("On the OpenDex computer. {}", tool.description),
                   "inputSchema":tool.schema})
        })
        .collect()
}

async fn handle(request: Request<Incoming>, state: Arc<Shared>) -> HttpReply {
    if request.headers().contains_key(header::ORIGIN) {
        return respond(StatusCode::FORBIDDEN, None);
    }
    let linked = match request.uri().path().trim_end_matches('/') {
        "/mcp" => None,
        path => {
            let ticket = path
                .strip_prefix("/mcp/")
                .and_then(|value| value.parse::<Uuid>().ok());
            match ticket {
                Some(ticket) if state.sender(ticket, Access::Link).is_some() => Some(ticket),
                _ => return respond(StatusCode::NOT_FOUND, None),
            }
        }
    };
    let modern = request
        .headers()
        .get("mcp-protocol-version")
        .is_some_and(|version| version == CURRENT);
    if request.method() != Method::POST {
        let mut response = respond(StatusCode::METHOD_NOT_ALLOWED, None);
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
        return response;
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.split(';').next() != Some("application/json"))
    {
        return respond(StatusCode::UNSUPPORTED_MEDIA_TYPE, None);
    }
    let body = Limited::new(request.into_body(), REQUEST_LIMIT).collect();
    let Ok(Ok(body)) = tokio::time::timeout(Duration::from_secs(10), body).await else {
        return respond(StatusCode::BAD_REQUEST, None);
    };
    let Ok(command) = serde_json::from_slice::<Value>(&body.to_bytes()) else {
        return respond(StatusCode::BAD_REQUEST, None);
    };
    let method = command["method"].as_str();
    if command["jsonrpc"] != "2.0" || method.is_none() {
        return respond(StatusCode::BAD_REQUEST, None);
    }
    let Some(id) = command.get("id") else {
        return respond(StatusCode::ACCEPTED, None);
    };
    if !(id.is_string() || id.is_i64() || id.is_u64()) {
        return respond(StatusCode::BAD_REQUEST, None);
    }
    let modern =
        modern || command["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] == CURRENT;
    let identity = json!({"name":"OpenDex Workspace","version":env!("CARGO_PKG_VERSION")});
    let mut result = match method {
        Some("initialize") => {
            let requested = command["params"]["protocolVersion"].as_str();
            let version = match requested {
                Some(version @ ("2025-03-26" | "2025-06-18")) => version,
                _ => LEGACY,
            };
            json!({"protocolVersion":version, "capabilities":{"tools":{}},
                   "serverInfo":identity})
        }
        Some("server/discover") => {
            json!({"supportedVersions":[CURRENT], "capabilities":{"tools":{}}})
        }
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools":tool_definitions(linked.is_some())}),
        Some("tools/call") => {
            let (ok, output) = invoke(&state, linked, &command["params"]).await;
            json!({"isError":!ok, "content":[{"type":"text","text":output}]})
        }
        _ => {
            return rpc_response(
                modern,
                json!({
                    "jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}
                }),
            );
        }
    };
    if modern {
        result["resultType"] = json!("complete");
        result["_meta"]["io.modelcontextprotocol/serverInfo"] = identity;
        if method == Some("tools/list") {
            result["cacheScope"] = json!("private");
            result["ttlMs"] = json!(0);
        }
    }
    rpc_response(modern, json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn rpc_response(modern: bool, frame: Value) -> HttpReply {
    if !modern {
        return respond(StatusCode::OK, Some(frame));
    }
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .header("mcp-protocol-version", CURRENT)
        .body(Full::new(Bytes::from(format!(
            "event: message\ndata: {frame}\n\n"
        ))))
        .expect("static HTTP response headers")
}

impl Shared {
    fn sender(&self, ticket: Uuid, access: Access) -> Option<mpsc::Sender<Invocation>> {
        self.active
            .lock()
            .expect("bridge lock")
            .get(&ticket)
            .filter(|(kind, _)| *kind == access)
            .map(|(_, send)| send.clone())
    }
}

async fn invoke(state: &Shared, linked: Option<Uuid>, params: &Value) -> (bool, String) {
    let Some(name) = params["name"]
        .as_str()
        .filter(|name| agent::capability(name).is_some())
    else {
        return (false, "Unknown OpenDex workspace tool.".into());
    };
    let Some(mut arguments) = params["arguments"].as_object().cloned() else {
        return (
            false,
            "OpenDex workspace tools require an arguments object.".into(),
        );
    };
    let ticket = arguments.remove("workspace_session").and_then(|value| {
        value
            .as_str()
            .and_then(|ticket| Uuid::parse_str(ticket).ok())
    });
    let sender = if let Some(link) = linked {
        state.sender(link, Access::Link)
    } else {
        ticket.and_then(|ticket| state.sender(ticket, Access::Turn))
    };
    let Some(sender) = sender else {
        return (
            false,
            "No approved OpenDex workspace is available for this turn.".into(),
        );
    };
    let (reply, result) = oneshot::channel();
    let call = ToolCall {
        id: Uuid::new_v4().to_string(),
        name: name.into(),
        arguments: Value::Object(arguments),
    };
    if sender.try_send(Invocation { call, reply }).is_err() {
        return (
            false,
            "The OpenDex workspace turn is busy or has ended.".into(),
        );
    }
    result
        .await
        .unwrap_or_else(|_| (false, "The OpenDex workspace turn ended.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_tools_require_a_live_grant_for_the_correct_route() {
        let bridge = Bridge::start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://{}/mcp", bridge.listen);
        let rpc = |method, params| {
            client.post(&url).json(&json!({
                "jsonrpc":"2.0", "id":1, "method":method, "params":params,
            }))
        };
        let browser = rpc("tools/list", json!({}))
            .header(header::ORIGIN, "https://untrusted.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(browser.status(), StatusCode::FORBIDDEN);
        let tools: Value = rpc("tools/list", json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"] == "read_file"
                    && entry["inputSchema"]["required"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("workspace_session")))
        );

        let (lease, mut work) = bridge.register(Access::Turn);
        let ticket = lease.ticket;
        let read = |ticket, path| {
            rpc(
                "tools/call",
                json!({
                    "name":"read_file", "arguments":{"workspace_session":ticket, "path":path}
                }),
            )
        };
        let denied: Value = read(Uuid::new_v4(), "proof.txt")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(denied["result"]["isError"], true);
        assert!(work.try_recv().is_err());

        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("proof.txt"), "local bridge fixture").unwrap();
        let path = agent::checked_root(project.path()).unwrap();
        let local = async {
            for _ in 0..2 {
                let Invocation { call, reply } = work.recv().await.unwrap();
                assert_eq!(call.name, "read_file");
                assert!(call.arguments.get("workspace_session").is_none());
                let result =
                    agent::execute(&path, &Permissions::default(), &call.name, &call.arguments)
                        .await;
                let result = match result {
                    Ok(output) => (true, output),
                    Err(error) => (false, error.to_string()),
                };
                assert!(reply.send(result).is_ok());
            }
        };
        let remote = async {
            let hit: Value = read(ticket, "proof.txt")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(hit["result"]["isError"], false);
            assert!(
                hit["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("local bridge fixture")
            );
            let outside: Value = read(ticket, "../proof.txt")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(outside["result"]["isError"], true);
        };
        tokio::join!(local, remote);

        drop(lease);
        let expired: Value = read(ticket, "proof.txt")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(expired["result"]["isError"], true);
        assert!(work.recv().await.is_none());

        let (link, mut external) = bridge.register(Access::Link);
        let invalid_route: Value = read(link.ticket, "proof.txt")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(invalid_route["result"]["isError"], true);
        let link_url = format!("{url}/{}", link.ticket);
        let discovery = client
            .post(&link_url)
            .header("mcp-protocol-version", CURRENT)
            .json(&json!({"jsonrpc":"2.0","id":2,"method":"server/discover","params":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            discovery.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert!(discovery.text().await.unwrap().contains(CURRENT));
        let linked_read = client.post(&link_url).json(&json!({
            "jsonrpc":"2.0", "id":3, "method":"tools/call",
            "params":{"name":"read_file","arguments":{"path":"proof.txt"}},
        }));
        let (response, ()) = tokio::join!(linked_read.send(), async {
            let invocation = external.recv().await.unwrap();
            assert_eq!(invocation.call.arguments, json!({"path":"proof.txt"}));
            assert!(
                invocation
                    .reply
                    .send((true, "external tool result".into()))
                    .is_ok()
            );
        });
        let result: Value = response.unwrap().json().await.unwrap();
        assert_eq!(
            result["result"]["content"][0]["text"],
            "external tool result"
        );
        drop(link);
        assert_eq!(
            client.post(&link_url).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }
}
