//! Transport for an installed, unmodified Claude Code executable.
//!
//! Claude Code owns authentication. OpenDex never reads its credential files.
//! Native tools, hooks, project settings, and external MCP discovery are
//! disabled. An in-process MCP bridge routes exposed tools through OpenDex
//! permission decisions. Initializing alone never sends an inference prompt.
//! Protocol reference: Anthropic's claude-agent-sdk-python internal/query.py.
use crate::{
    mcp::{bounded_line, process_path},
    providers::Tool,
    state::{Message, Role, ToolCall},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{future::Future, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

pub struct Bridge {
    _process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _scratch: tempfile::TempDir,
    initialization: Value,
    tools: Vec<Tool>,
}

impl Bridge {
    /// `config_home` is dedicated to OpenDex, independent of terminal sessions.
    /// Initialization does not submit a prompt or start a paid inference turn.
    pub async fn start(program: &str, config_home: &Path) -> Result<Self> {
        Self::start_turn(program, config_home, None, "", &[]).await
    }

    pub async fn start_turn(
        program: &str,
        config_home: &Path,
        model: Option<&str>,
        system: &str,
        tools: &[Tool],
    ) -> Result<Self> {
        ensure!(
            !program.trim().is_empty(),
            "Set the Claude Code executable path."
        );
        #[cfg(windows)]
        ensure!(
            Path::new(program)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
            "Use the native Claude Code .exe on Windows."
        );
        std::fs::create_dir_all(config_home)?;
        let config_home = config_home.canonicalize()?;
        let scratch = tempfile::Builder::new()
            .prefix("opendex-claude-")
            .tempdir()?;
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", process_path())
            .env("CLAUDE_CONFIG_DIR", config_home);
        if let Some(base) = directories::BaseDirs::new() {
            command.env("HOME", base.home_dir());
        }
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let config = if tools.is_empty() {
            json!({"mcpServers":{}})
        } else {
            json!({"mcpServers":{"opendex":{"type":"sdk","name":"opendex"}}})
        };
        command.args([
            "--mcp-config",
            &config.to_string(),
            "--system-prompt",
            system,
        ]);
        if let Some(model) = model {
            command.args(["--model", model]);
        }
        for tool in tools {
            ensure!(
                !tool.name.is_empty()
                    && tool
                        .name
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'),
                "Invalid Claude Code tool name."
            );
        }
        if !tools.is_empty() {
            let allowed = tools
                .iter()
                .map(|tool| format!("mcp__opendex__{}", tool.name))
                .collect::<Vec<_>>()
                .join(",");
            command.args(["--allowedTools", &allowed]);
        }
        let mut process = command
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--no-session-persistence",
                "--tools",
                "",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--disable-slash-commands",
                "--no-chrome",
                "--settings",
                "{\"disableAllHooks\":true,\"autoMemoryEnabled\":false,\"enabledPlugins\":{}}",
            ])
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Could not start Claude Code. Install it or set its full executable path.")?;
        let stdin = process
            .stdin
            .take()
            .context("Claude Code stdin unavailable.")?;
        let stdout = BufReader::new(
            process
                .stdout
                .take()
                .context("Claude Code stdout unavailable.")?,
        );
        let mut bridge = Self {
            _process: process,
            stdin,
            stdout,
            _scratch: scratch,
            initialization: Value::Null,
            tools: tools.to_vec(),
        };
        bridge
            .send(
                json!({"type":"control_request","request_id":"opendex-initialize",
            "request":{"subtype":"initialize","hooks":null,"skills":[]}}),
            )
            .await?;
        bridge.initialization = timeout(Duration::from_secs(60), bridge.initialize())
            .await
            .context("Claude Code did not initialize within a minute.")??;
        Ok(bridge)
    }

    /// CLI-provided capabilities, including its selectable models when available.
    pub fn initialization(&self) -> &Value {
        &self.initialization
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn initialize(&mut self) -> Result<Value> {
        loop {
            let line = bounded_line(&mut self.stdout)
                .await
                .context("Claude Code closed its output during initialization.")?;
            let message: Value = serde_json::from_slice(&line)
                .context("Claude Code returned invalid protocol JSON.")?;
            match message["type"].as_str() {
                Some("control_request") => {
                    if let Some(result) = self.mcp_metadata(&message) {
                        self.reply_control(&message, result).await?;
                        continue;
                    }
                    // No tool execution is authorized during initialization.
                    let id = message
                        .get("request_id")
                        .context("Claude Code control request has no ID.")?;
                    self.send(json!({"type":"control_response","response":{
                        "subtype":"error","request_id":id,
                        "error":"OpenDex does not authorize native operations during initialization."
                    }})).await?;
                }
                Some("control_response")
                    if message["response"]["request_id"] == "opendex-initialize" =>
                {
                    let response = &message["response"];
                    ensure!(
                        response["subtype"] == "success",
                        "Claude Code could not initialize its control protocol."
                    );
                    return response
                        .get("response")
                        .filter(|value| value.is_object())
                        .cloned()
                        .context("Claude Code initialization returned no capabilities.");
                }
                Some("result") => bail!(
                    "Claude Code ended before initialization. Check its installation and configuration."
                ),
                _ => {}
            }
        }
    }

    async fn incoming(&mut self) -> Result<Value> {
        let line = bounded_line(&mut self.stdout)
            .await
            .context("Claude Code closed its output.")?;
        serde_json::from_slice(&line).context("Claude Code returned invalid protocol JSON.")
    }

    fn mcp_metadata(&self, message: &Value) -> Option<Value> {
        let request = &message["request"];
        if request["subtype"] != "mcp_message" || request["server_name"] != "opendex" {
            return None;
        }
        let rpc = &request["message"];
        let result = match rpc["method"].as_str()? {
            "initialize" => {
                json!({"protocolVersion":rpc["params"]["protocolVersion"], "capabilities":{"tools":{}}, "serverInfo":{"name":"opendex","version":env!("CARGO_PKG_VERSION")}})
            }
            "notifications/initialized" => return Some(json!({"jsonrpc":"2.0","result":{}})),
            "ping" => json!({}),
            "tools/list" => {
                json!({"tools":self.tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"inputSchema":tool.schema})).collect::<Vec<_>>()})
            }
            _ => return None,
        };
        Some(json!({"jsonrpc":"2.0","id":rpc["id"],"result":result}))
    }

    async fn reply_control(&mut self, message: &Value, rpc: Value) -> Result<()> {
        self.send(json!({"type":"control_response","response":{"subtype":"success", "request_id":message["request_id"], "response":{"mcp_response":rpc}}})).await
    }

    /// All tool executions re-enter the caller's permission and persistence layer.
    /// Callers must drop the bridge when cancelling a turn. Its child uses `kill_on_drop`.
    pub async fn generate<F: Future<Output = (bool, String)>>(
        &mut self,
        messages: &[Message],
        mut delta: impl FnMut(&str),
        mut call_tool: impl FnMut(ToolCall) -> F,
    ) -> Result<String> {
        let history: Vec<_> = messages
            .iter()
            .filter(|msg| msg.role != Role::Tool && msg.has_content())
            .map(|msg| json!({"role":msg.role,"content":msg.content()}))
            .collect();
        ensure!(!history.is_empty(), "Write a message before sending.");
        let prompt = if history.len() == 1 {
            history[0]["content"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        } else {
            format!(
                "Continue this OpenDex conversation. Answer its last user message.\n{}",
                serde_json::to_string(&history)?
            )
        };
        let mut content = vec![json!({"type":"text","text":prompt})];
        for (file, mime) in messages.iter().flat_map(Message::media_attachments) {
            let kind = if mime.starts_with("image/") {
                "image"
            } else if mime == "application/pdf" {
                "document"
            } else {
                bail!("Claude Code cannot accept this attachment format.")
            };
            content.push(
                json!({"type":kind,"source":{"type":"base64","media_type":mime,"data":file.text}}),
            );
        }
        self.send(json!({"type":"user","session_id":"","parent_tool_use_id":null,"client_composed":true,"message":{"role":"user","content":content}})).await?;
        let mut text = String::new();
        let mut segment_streamed = false;
        loop {
            let message = timeout(Duration::from_secs(150), self.incoming())
                .await
                .context("Claude Code response timed out.")??;
            match message["type"].as_str() {
                Some("control_request") => {
                    if let Some(result) = self.mcp_metadata(&message) {
                        self.reply_control(&message, result).await?;
                        continue;
                    }
                    let request = &message["request"];
                    if request["subtype"] == "mcp_message"
                        && request["server_name"] == "opendex"
                        && request["message"]["method"] == "tools/call"
                    {
                        let rpc = &request["message"];
                        let name = rpc["params"]["name"].as_str().unwrap_or_default();
                        let (success, output) = if self.tools.iter().any(|tool| tool.name == name) {
                            call_tool(ToolCall {
                                id: uuid::Uuid::new_v4().to_string(),
                                name: name.into(),
                                arguments: rpc["params"]["arguments"].clone(),
                            })
                            .await
                        } else {
                            (false, "This tool was not exposed by OpenDex.".into())
                        };
                        self.reply_control(&message, json!({"jsonrpc":"2.0","id":rpc["id"],"result":{"isError":!success,"content":[{"type":"text","text":output}]}})).await?;
                    } else {
                        self.send(json!({"type":"control_response","response":{"subtype":"error","request_id":message["request_id"],"error":"This native operation is not authorized by OpenDex."}})).await?;
                    }
                }
                Some("stream_event") if message["parent_tool_use_id"].is_null() => {
                    if message["event"]["delta"]["type"] == "text_delta"
                        && let Some(piece) = message["event"]["delta"]["text"].as_str()
                    {
                        append_text(&mut text, piece, &mut delta)?;
                        segment_streamed = true;
                    }
                }
                Some("assistant") if message["parent_tool_use_id"].is_null() => {
                    if !segment_streamed
                        && let Some(blocks) = message["message"]["content"].as_array()
                    {
                        for block in blocks {
                            if block["type"] == "text"
                                && let Some(piece) = block["text"].as_str()
                            {
                                append_text(&mut text, piece, &mut delta)?;
                            }
                        }
                    }
                    segment_streamed = false;
                }
                Some("result") => {
                    ensure!(
                        message["is_error"] != true && message["subtype"] == "success",
                        "Claude Code could not complete the turn. Check its account connection or usage limits."
                    );
                    if text.is_empty()
                        && let Some(piece) = message["result"].as_str()
                    {
                        append_text(&mut text, piece, &mut delta)?;
                    }
                    return Ok(text);
                }
                _ => {}
            }
        }
    }
}

fn append_text(text: &mut String, piece: &str, delta: &mut impl FnMut(&str)) -> Result<()> {
    ensure!(
        text.len() + piece.len() <= 4 * 1024 * 1024,
        "Claude Code response exceeds the size limit."
    );
    text.push_str(piece);
    delta(piece);
    Ok(())
}
