//! MCP tools, resources, and prompts over stdio and Streamable HTTP.
use crate::{
    providers::{Tool, checked, frames},
    state::{MAX_RESOURCE_BYTES, Server, Transport, endpoint, read_secret},
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    path::Path,
    process::Stdio,
    time::Duration,
};
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
    headers: Vec<(header::HeaderName, header::HeaderValue)>,
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
    capabilities: Option<Value>,
}

#[derive(Debug)]
struct RpcError {
    server: String,
    code: Option<i64>,
    message: String,
}

impl fmt::Display for RpcError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "MCP {}: {}", self.server, self.message)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Deserialize, Serialize)]
pub struct Resource {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub uri: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub uri_template: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PromptArgument {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PromptTemplate {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Default, Serialize)]
pub struct ContextCatalog {
    pub can_read_resources: bool,
    pub resources: Vec<Resource>,
    pub templates: Vec<ResourceTemplate>,
    pub prompts: Vec<PromptTemplate>,
}

#[derive(Debug, Serialize)]
pub struct ResourcePreview {
    pub text: String,
    pub binary_count: usize,
}

#[derive(Debug, Serialize)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
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
                for entry in &server.env {
                    let (name, fixed) = crate::state::split_env_assignment(entry)
                        .context("Invalid MCP environment setup.")?;
                    let value = match fixed {
                        Some(value) => value.into(),
                        None => std::env::var_os(name).with_context(|| {
                            format!("MCP environment variable {name} is unavailable.")
                        })?,
                    };
                    process.env(name, value);
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
                secret: read_secret(server.credential_id(), &server.address, server.has_secret)?,
                session: None,
                headers: streamable_headers(server)?,
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
            capabilities: None,
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
            connection.capabilities = Some(response["capabilities"].clone());
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
            return Err(RpcError {
                server: self.server.name.clone(),
                code: error["code"].as_i64(),
                message: error["message"].as_str().unwrap_or("request failed").into(),
            }
            .into());
        }
        let result = response
            .get("result")
            .cloned()
            .context("MCP response is missing a result.")?;
        ensure!(
            result["resultType"] != "input_required",
            "This MCP operation requires an interactive input round trip."
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
        let mirrored = match method {
            "resources/read" => params["uri"].as_str(),
            "tools/call" | "prompts/get" => params["name"].as_str(),
            _ => None,
        }
        .map(str::to_owned);
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
                    for (name, value) in &http.headers {
                        post = post.header(name.clone(), value.clone());
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
                        let mut body = raw.bytes_stream();
                        let mut bytes = Vec::new();
                        use futures_util::StreamExt;
                        while let Some(chunk) = body.next().await {
                            let chunk = chunk?;
                            ensure!(
                                bytes.len() + chunk.len() <= MAX_FRAME as usize,
                                "JSON-RPC message exceeds the size limit."
                            );
                            bytes.extend_from_slice(&chunk);
                        }
                        let value: Value = serde_json::from_slice(&bytes)?;
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

    async fn list<T: DeserializeOwned>(&mut self, method: &str, field: &str) -> Result<Vec<T>> {
        let mut params = json!({});
        let mut cursors = HashSet::new();
        let mut items = Vec::new();
        for _ in 0..12 {
            let mut result = self.request(method, params.clone()).await?;
            let page = result
                .get_mut(field)
                .with_context(|| format!("MCP response is missing {field}."))?
                .take();
            let page: Vec<T> = serde_json::from_value(page)
                .with_context(|| format!("Invalid data returned by {method}."))?;
            ensure!(
                items.len() + page.len() <= 2000,
                "MCP catalog exceeds the item limit."
            );
            items.extend(page);
            let Some(cursor) = result["nextCursor"].as_str() else {
                return Ok(items);
            };
            ensure!(
                cursors.insert(cursor.to_owned()),
                "MCP server returned a repeated page cursor."
            );
            params["cursor"] = json!(cursor);
        }
        bail!("MCP catalog exceeds the page limit.")
    }

    pub async fn tools(&mut self) -> Result<Vec<Tool>> {
        let items: Vec<Value> = self.list("tools/list", "tools").await?;
        Ok(items
            .into_iter()
            .filter_map(|item| {
                Some(Tool {
                    name: item["name"].as_str()?.into(),
                    description: item["description"].as_str().unwrap_or_default().into(),
                    schema: item["inputSchema"]
                        .as_object()
                        .map(|_| item["inputSchema"].clone())?,
                })
            })
            .collect())
    }

    pub async fn context_catalog(&mut self) -> Result<ContextCatalog> {
        // Connections using initialize already have the server's capabilities.
        if self.capabilities.is_none() {
            let mut discovered = self.request("server/discover", json!({})).await?;
            self.capabilities = Some(discovered["capabilities"].take());
        }
        let capabilities = self.capabilities.as_ref().unwrap_or(&Value::Null);
        let has_resources = capabilities["resources"].is_object();
        let has_prompts = capabilities["prompts"].is_object();
        let mut catalog = ContextCatalog {
            can_read_resources: has_resources,
            ..ContextCatalog::default()
        };
        if has_resources {
            catalog.resources = self.list("resources/list", "resources").await?;
            catalog.templates = match self
                .list("resources/templates/list", "resourceTemplates")
                .await
            {
                Err(error)
                    if error
                        .downcast_ref::<RpcError>()
                        .is_some_and(|error| error.code == Some(-32601)) =>
                {
                    vec![]
                }
                result => result?,
            };
        }
        if has_prompts {
            catalog.prompts = self.list("prompts/list", "prompts").await?;
        }
        Ok(catalog)
    }

    pub async fn read_resource(&mut self, uri: &str) -> Result<ResourcePreview> {
        ensure!(
            uri.len() <= 2048
                && reqwest::Url::parse(uri).is_ok()
                && !uri.chars().any(char::is_control),
            "Enter a valid absolute resource URI."
        );
        // Even file: and https: identifiers are passed to the selected MCP
        // server. The client never opens those paths or URLs directly.
        let result = self.request("resources/read", json!({"uri":uri})).await?;
        let contents = result["contents"]
            .as_array()
            .context("MCP server did not return resource contents.")?;
        let mut text = String::new();
        let mut binary_count = 0;
        for content in contents {
            let Some(body) = content["text"].as_str() else {
                binary_count += 1;
                continue;
            };
            ensure!(
                text.len() + body.len() <= MAX_RESOURCE_BYTES,
                "This resource exceeds the text size limit."
            );
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            let source = content["uri"].as_str().unwrap_or(uri);
            if contents.len() > 1 {
                text.push_str(&format!("[{source}]\n"));
            }
            text.push_str(body);
            ensure!(
                text.len() <= MAX_RESOURCE_BYTES,
                "This resource exceeds the text size limit."
            );
        }
        ensure!(
            !text.trim().is_empty(),
            "This resource contains no text to attach."
        );
        Ok(ResourcePreview { text, binary_count })
    }

    pub async fn get_prompt(
        &mut self,
        name: &str,
        arguments: BTreeMap<String, String>,
    ) -> Result<Vec<PromptMessage>> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
            "Invalid MCP prompt name."
        );
        ensure!(
            arguments.len() <= 64
                && arguments.keys().all(|key| key.len() <= 256)
                && arguments.values().map(String::len).sum::<usize>() <= MAX_RESOURCE_BYTES,
            "MCP prompt arguments exceed the size limit."
        );
        let result = self
            .request("prompts/get", json!({"name":name,"arguments":arguments}))
            .await?;
        let messages = result["messages"]
            .as_array()
            .context("MCP server did not return prompt messages.")?;
        ensure!(
            messages.len() <= 64,
            "This MCP prompt has too many messages."
        );
        let mut preview = Vec::with_capacity(messages.len());
        let mut size = 0;
        for message in messages {
            let role = message["role"]
                .as_str()
                .filter(|role| matches!(*role, "user" | "assistant"))
                .context("MCP prompt has an invalid message role.")?;
            let content = &message["content"];
            let text = match content["type"].as_str() {
                Some("text") => content["text"].as_str(),
                Some("resource") => content["resource"]["text"].as_str(),
                _ => None,
            }.context("This MCP prompt contains media or a resource link. OpenDex can insert prompts with text and embedded text resources.")?;
            size += text.len();
            ensure!(
                size <= MAX_RESOURCE_BYTES,
                "This MCP prompt exceeds the text size limit."
            );
            preview.push(PromptMessage {
                role: role.into(),
                text: text.into(),
            });
        }
        ensure!(
            preview
                .iter()
                .any(|message| !message.text.trim().is_empty()),
            "This MCP prompt contains no text to use."
        );
        Ok(preview)
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

fn streamable_headers(server: &Server) -> Result<Vec<(header::HeaderName, header::HeaderValue)>> {
    server
        .headers
        .iter()
        .map(|(name, variable)| {
            let value = std::env::var(variable)
                .with_context(|| format!("MCP environment variable {variable} is unavailable."))?;
            let value = header::HeaderValue::from_str(&value).with_context(|| {
                format!("MCP environment variable {variable} contains an invalid header value.")
            })?;
            let name = header::HeaderName::from_bytes(name.as_bytes())
                .context("MCP header name is invalid.")?;
            Ok((name, value))
        })
        .collect()
}
