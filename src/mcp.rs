//! MCP tools over local stdio and remote Streamable HTTP.
use crate::{
    providers::{Tool, checked, frames},
    state::{Server, Transport, endpoint, read_secret},
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, header};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

const LEGACY: &str = "2025-11-25";
const CURRENT: &str = "2026-07-28";
const MAX_FRAME: u64 = 4 * 1024 * 1024;

/// Finder-launched apps have a smaller PATH than interactive terminals.
pub fn process_path() -> std::ffi::OsString {
    let mut paths = std::env::var_os("PATH")
        .as_deref()
        .map(std::env::split_paths)
        .map(|paths| paths.collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = directories::BaseDirs::new() {
        paths.push(home.home_dir().join(".local/bin"));
        paths.push(home.home_dir().join(".cargo/bin"));
    }
    #[cfg(unix)]
    paths.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(std::path::PathBuf::from),
    );
    std::env::join_paths(paths).unwrap_or_default()
}

pub async fn bounded_line(reader: &mut BufReader<impl AsyncRead + Unpin>) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    reader
        .take(MAX_FRAME + 1)
        .read_until(b'\n', &mut line)
        .await?;
    ensure!(
        line.len() as u64 <= MAX_FRAME,
        "JSON-RPC message exceeds the size limit."
    );
    ensure!(!line.is_empty(), "The subprocess closed its output.");
    Ok(line)
}

struct Process {
    _child: Child,
    send: ChildStdin,
    receive: BufReader<ChildStdout>,
}

struct Streamable {
    client: Client,
    url: reqwest::Url,
    secret: Option<String>,
    session: Option<header::HeaderValue>,
}

enum Channel {
    Process(Box<Process>),
    Http(Box<Streamable>),
}

pub struct Connection {
    channel: Channel,
    id: u64,
    pub version: String,
    pub server: Server,
}

fn identity() -> Value {
    json!({"name":"OpenDex", "version":env!("CARGO_PKG_VERSION")})
}

impl Connection {
    pub async fn connect(client: &Client, server: &Server, root: Option<&Path>) -> Result<Self> {
        let channel = match server.transport {
            Transport::Stdio => {
                let mut process = Command::new(&server.address);
                process.env_clear().env("PATH", process_path());
                for variable in &server.env {
                    let value = std::env::var_os(variable).with_context(|| {
                        format!("MCP environment variable {variable} is unavailable.")
                    })?;
                    process.env(variable, value);
                }
                process
                    .args(&server.args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                if let Some(root) = root {
                    process.current_dir(root);
                }
                let mut child = process
                    .spawn()
                    .with_context(|| format!("Could not start {}.", server.name))?;
                let send = child.stdin.take().context("Missing MCP stdin.")?;
                let receive = BufReader::new(child.stdout.take().context("Missing MCP stdout.")?);
                Channel::Process(Box::new(Process {
                    _child: child,
                    send,
                    receive,
                }))
            }
            Transport::Http => Channel::Http(Box::new(Streamable {
                client: client.clone(),
                url: endpoint(&server.address)?,
                secret: read_secret(server.id, &server.address, server.has_secret)?,
                session: None,
            })),
        };
        let mut connection = Self {
            channel,
            id: 0,
            version: if server.current_protocol {
                CURRENT
            } else {
                LEGACY
            }
            .into(),
            server: server.clone(),
        };
        if !server.current_protocol {
            let response = connection
                .request(
                    "initialize",
                    json!({
                        "protocolVersion":LEGACY, "capabilities":{}, "clientInfo":identity()
                    }),
                )
                .await?;
            let version = response["protocolVersion"]
                .as_str()
                .context("MCP server did not negotiate a protocol version.")?;
            ensure!(
                matches!(version, LEGACY | "2025-06-18" | "2025-03-26"),
                "Unsupported MCP version {version}. Check the server's protocol setting."
            );
            connection.version = version.into();
            connection
                .send("notifications/initialized", json!({}), None)
                .await?;
        }
        Ok(connection)
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let id = self.id;
        let response = self
            .send(method, params, Some(id))
            .await?
            .context("MCP server returned no response.")?;
        if let Some(error) = response.get("error") {
            bail!(
                "MCP {}: {}",
                self.server.name,
                error["message"].as_str().unwrap_or("request failed")
            );
        }
        let result = response
            .get("result")
            .cloned()
            .context("MCP response is missing a result.")?;
        ensure!(
            result["resultType"] != "input_required",
            "This MCP tool requires an interactive input round trip."
        );
        Ok(result)
    }

    async fn send(
        &mut self,
        method: &str,
        mut params: Value,
        id: Option<u64>,
    ) -> Result<Option<Value>> {
        if self.version == CURRENT {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": CURRENT,
                "io.modelcontextprotocol/clientInfo": identity(),
                "io.modelcontextprotocol/clientCapabilities": {}
            });
        }
        let mirrored = params["name"].as_str().map(str::to_owned);
        let mut message = json!({"jsonrpc":"2.0","method":method,"params":params});
        if let Some(id) = id {
            message["id"] = json!(id);
        }
        let request = async {
            match &mut self.channel {
                Channel::Process(process) => {
                    let mut msg = serde_json::to_vec(&message)?;
                    msg.push(b'\n');
                    process.send.write_all(&msg).await?;
                    process.send.flush().await?;
                    let Some(id) = id else {
                        return Ok(None);
                    };
                    loop {
                        let line = bounded_line(&mut process.receive).await?;
                        let response: Value = serde_json::from_slice(&line)
                            .context("MCP stdout must contain JSON-RPC messages.")?;
                        if response["id"].as_u64() == Some(id) && response.get("method").is_none() {
                            return Ok(Some(response));
                        }
                        // Reject unadvertised server requests so a server waiting for a reply
                        // does not hold the client request open indefinitely.
                        if let Some(request_id) = response
                            .get("id")
                            .filter(|_| response.get("method").is_some())
                        {
                            let reply = json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Client method unavailable"}});
                            let mut bytes = serde_json::to_vec(&reply)?;
                            bytes.push(b'\n');
                            process.send.write_all(&bytes).await?;
                            process.send.flush().await?;
                        }
                    }
                }
                Channel::Http(http) => {
                    let mut post = http
                        .client
                        .post(http.url.clone())
                        .header(header::ACCEPT, "application/json, text/event-stream")
                        .header("MCP-Protocol-Version", &self.version)
                        .json(&message);
                    if self.version == CURRENT {
                        post = post.header("Mcp-Method", method);
                        if let Some(name) = mirrored {
                            post = post.header("Mcp-Name", name);
                        }
                    }
                    if let Some(secret) = &http.secret {
                        post = post.bearer_auth(secret);
                    }
                    if let Some(session) = &http.session {
                        post = post.header("Mcp-Session-Id", session);
                    }
                    let raw = post.send().await?;
                    let raw = if self.version == CURRENT
                        && raw.status().as_u16() == 400
                        && raw
                            .headers()
                            .get(header::CONTENT_TYPE)
                            .is_some_and(|t| t.as_bytes().starts_with(b"application/json"))
                    {
                        raw
                    } else {
                        checked(raw).await?
                    };
                    if method == "initialize" {
                        http.session = raw.headers().get("Mcp-Session-Id").cloned();
                    }
                    let Some(id) = id else {
                        return Ok(None);
                    };
                    let sse = raw
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"));
                    if !sse {
                        let value: Value = raw.json().await?;
                        ensure!(
                            value["id"].as_u64() == Some(id),
                            "MCP response had an unexpected ID."
                        );
                        return Ok(Some(value));
                    }
                    let mut result = None;
                    frames(raw, true, |frame| {
                        let received: Value = serde_json::from_str(frame)?;
                        if received["id"].as_u64() == Some(id) && received.get("method").is_none() {
                            result = Some(received);
                        }
                        Ok(result.is_some())
                    })
                    .await?;
                    Ok(result)
                }
            }
        };
        timeout(
            Duration::from_secs(if method == "tools/call" { 120 } else { 20 }),
            request,
        )
        .await
        .with_context(|| format!("{} timed out on {method}.", self.server.name))?
    }

    pub async fn tools(&mut self) -> Result<Vec<Tool>> {
        let mut params = json!({});
        let mut items = Vec::new();
        for _ in 0..12 {
            let result = self.request("tools/list", params.clone()).await?;
            let tools = result["tools"]
                .as_array()
                .context("MCP server did not return tools.")?;
            items.extend(tools.iter().filter_map(|item| {
                Some(Tool {
                    name: item["name"].as_str()?.into(),
                    description: item["description"].as_str().unwrap_or_default().into(),
                    schema: item["inputSchema"]
                        .as_object()
                        .map(|_| item["inputSchema"].clone())?,
                })
            }));
            let Some(cursor) = result["nextCursor"].as_str() else {
                break;
            };
            params["cursor"] = json!(cursor);
        }
        Ok(items)
    }

    pub async fn call(&mut self, name: &str, arguments: Value) -> Result<String> {
        let result = self
            .request("tools/call", json!({"name":name,"arguments":arguments}))
            .await?;
        // Text-only model adapters receive text or structured content. Binary data
        // stays out of the transcript and the next model prompt.
        let texts: Vec<_> = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect();
        let output = if let Some(structured) = result.get("structuredContent") {
            structured.to_string()
        } else if texts.is_empty() {
            "MCP returned a non-text result.".into()
        } else {
            texts.join("\n")
        };
        let prefix = if result["isError"] == true {
            "MCP tool error: "
        } else {
            ""
        };
        Ok(format!(
            "{prefix}{}",
            output.chars().take(48_000).collect::<String>()
        ))
    }

    pub async fn close(self) {
        if let Channel::Http(http) = self.channel
            && let Some(session) = http.session
        {
            let mut delete = http
                .client
                .delete(http.url)
                .header("Mcp-Session-Id", session)
                .header("MCP-Protocol-Version", self.version);
            if let Some(secret) = http.secret {
                delete = delete.bearer_auth(secret);
            }
            let _ = timeout(Duration::from_secs(2), delete.send()).await;
        }
    }
}
