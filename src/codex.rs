//! Optional ChatGPT account integration through the installed Codex app server.
//! Codex owns the browser flow and credential lifecycle.
use crate::{
    mcp::{bounded_line, process_path},
    providers::Tool,
    state::{Message, Role, ToolCall},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{future::Future, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub connected: bool,
    pub label: String,
}

pub struct Bridge {
    _process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    _scratch: tempfile::TempDir,
}

impl Bridge {
    pub async fn start(program: &str, codex_home: &Path) -> Result<Self> {
        ensure!(
            !program.is_empty(),
            "Set the Codex CLI path in Provider settings."
        );
        std::fs::create_dir_all(codex_home)?;
        let scratch = tempfile::Builder::new()
            .prefix("opendex-codex-")
            .tempdir()?;
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", process_path())
            .env("CODEX_HOME", codex_home);
        if let Some(base) = directories::BaseDirs::new() {
            command.env("HOME", base.home_dir());
        }
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let mut process = command
            .args([
                "app-server",
                "--listen",
                "stdio://",
                "-c",
                "cli_auth_credentials_store=\"keyring\"",
                "-c",
                "analytics.enabled=false",
                "-c",
                "web_search=\"disabled\"",
                "-c",
                "features.shell_tool=false",
                "-c",
                "features.unified_exec=false",
                "-c",
                "features.apps=false",
                "-c",
                "features.plugins=false",
                "-c",
                "features.remote_plugin=false",
                "-c",
                "mcp_servers={}",
            ])
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context(
                "Could not start the Codex CLI. Install it or set its full path in Settings.",
            )?;
        let stdin = process.stdin.take().context("Codex stdin unavailable.")?;
        let stdout = BufReader::new(process.stdout.take().context("Codex stdout unavailable.")?);
        let mut bridge = Self {
            _process: process,
            stdin,
            stdout,
            next: 0,
            _scratch: scratch,
        };
        bridge.rpc("initialize", json!({
            "clientInfo":{"name":"opendex","title":"OpenDex","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true}
        })).await.context("Codex app-server could not initialize. Check the CLI version.")?;
        bridge
            .send(json!({"method":"initialized","params":{}}))
            .await?;
        Ok(bridge)
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn incoming(&mut self) -> Result<Value> {
        let line = bounded_line(&mut self.stdout).await?;
        serde_json::from_slice(&line).context("The Codex app server returned invalid JSON.")
    }

    async fn reject(&mut self, id: &Value) -> Result<()> {
        self.send(json!({"id":id,"error":{"code":-32601,"message":"OpenDex does not authorize this native Codex operation."}})).await
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(json!({"method":method,"id":id,"params":params}))
            .await?;
        timeout(Duration::from_secs(45), async {
            loop {
                let value = self.incoming().await?;
                if let Some(id) = value.get("id").filter(|_| value.get("method").is_some()) {
                    self.reject(id).await?;
                    continue;
                }
                if value["id"].as_u64() == Some(id) {
                    if let Some(error) = value["error"]["message"].as_str() {
                        bail!("Codex: {error}");
                    }
                    return value
                        .get("result")
                        .cloned()
                        .context("Codex RPC returned no result.");
                }
            }
        })
        .await
        .context("Codex app server did not reply.")?
    }

    pub async fn account(&mut self) -> Result<Account> {
        let result = self
            .rpc("account/read", json!({"refreshToken":false}))
            .await?;
        let account = &result["account"];
        let connected = account["type"] == "chatgpt";
        let label = if connected {
            account["email"]
                .as_str()
                .unwrap_or("ChatGPT account")
                .to_owned()
        } else {
            "Disconnected".into()
        };
        Ok(Account { connected, label })
    }

    pub async fn logout(&mut self) -> Result<()> {
        self.rpc("account/logout", json!({})).await?;
        Ok(())
    }

    pub async fn login(&mut self, opened: impl FnOnce(&str)) -> Result<Account> {
        let response = self
            .rpc("account/login/start", json!({"type":"chatgpt"}))
            .await?;
        let url = response["authUrl"]
            .as_str()
            .context("Codex did not return a sign-in URL.")?;
        let id = response["loginId"]
            .as_str()
            .context("Codex did not return a sign-in ID.")?;
        let url = reqwest::Url::parse(url)?;
        ensure!(
            url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some("auth.openai.com" | "chatgpt.com" | "auth0.openai.com")
                )
                && url.username().is_empty()
                && url.password().is_none(),
            "The Codex sign-in URL is outside the expected OpenAI hosts."
        );
        opened(url.as_str());
        timeout(Duration::from_secs(300), async {
            loop {
                let message = self.incoming().await?;
                if message["method"] == "account/login/completed"
                    && message["params"]["loginId"] == id
                {
                    let params = &message["params"];
                    ensure!(
                        params["success"] == true,
                        "{}",
                        params["error"]
                            .as_str()
                            .unwrap_or("ChatGPT sign-in failed.")
                    );
                    return self.account().await;
                }
            }
        })
        .await
        .context("The browser sign-in expired.")?
    }

    pub async fn models(&mut self) -> Result<Vec<String>> {
        ensure!(
            self.account().await?.connected,
            "Connect your ChatGPT account first."
        );
        let mut model_ids = Vec::new();
        let mut params = json!({"limit":100,"includeHidden":false});
        for _ in 0..10 {
            let response = self.rpc("model/list", params.clone()).await?;
            let data = response["data"]
                .as_array()
                .context("Codex returned an incompatible model list.")?;
            model_ids.extend(
                data.iter()
                    .filter_map(|item| item["id"].as_str().map(str::to_owned)),
            );
            let Some(cursor) = response["nextCursor"].as_str() else {
                break;
            };
            params["cursor"] = json!(cursor);
        }
        Ok(model_ids)
    }

    /// Keep Codex native tools in an empty, read-only directory. Code Agent
    /// and MCP actions re-enter OpenDex via the advertised dynamic tools.
    pub async fn generate<F: Future<Output = (bool, String)>>(
        &mut self,
        model: &str,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        mut delta: impl FnMut(&str),
        mut call_tool: impl FnMut(ToolCall) -> F,
    ) -> Result<String> {
        ensure!(
            self.account().await?.connected,
            "Connect your ChatGPT account first."
        );
        let cwd = self._scratch.path().to_path_buf();
        let dynamic: Vec<_> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type":"function","name":tool.name,"description":tool.description,
                    "inputSchema":tool.schema,"deferLoading":false,
                })
            })
            .collect();
        let result = self.rpc("thread/start", json!({
            "cwd":cwd, "model":model, "approvalPolicy":"never", "sandbox":"read-only",
            "baseInstructions":"You are the assistant in OpenDex. Use only tools supplied by OpenDex and follow its permission decisions.",
            "developerInstructions":system, "ephemeral":true, "serviceName":"OpenDex",
            "dynamicTools":dynamic,
        })).await?;
        let id = result["thread"]["id"]
            .as_str()
            .context("Codex did not return a thread ID.")?;
        let context: Vec<_> = messages
            .iter()
            .filter(|msg| msg.role != Role::Tool && msg.has_content())
            .map(|msg| json!({"role":msg.role,"content":msg.content()}))
            .collect();
        let input = if context.len() == 1 {
            context[0]["content"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        } else {
            format!(
                "Continue this OpenDex conversation. Answer its last user message.\n{}",
                serde_json::to_string(&context)?
            )
        };
        let mut input = vec![json!({"type":"text","text":input,"text_elements":[]})];
        for (file, _) in messages
            .iter()
            .flat_map(Message::media_attachments)
            .filter(|(_, mime)| mime.starts_with("image/"))
        {
            input.push(json!({"type":"image","url":file.data_url()}));
        }
        // Read notification messages from the moment turn/start is sent. Some
        // app-server versions publish early turn events before the RPC reply.
        self.next += 1;
        let request_id = self.next;
        self.send(json!({"method":"turn/start","id":request_id,"params":{
            "threadId":id, "input":input,
            "sandboxPolicy":{"type":"readOnly","networkAccess":false},"approvalPolicy":"never"
        }}))
        .await?;

        let mut text = String::new();
        let mut failure = String::new();
        loop {
            let value = timeout(Duration::from_secs(150), self.incoming())
                .await
                .context("The Codex turn timed out.")??;
            if value["id"].as_u64() == Some(request_id)
                && let Some(error) = value["error"]["message"].as_str()
            {
                bail!("Codex: {error}");
            }
            let params = &value["params"];
            match value["method"].as_str() {
                Some("item/agentMessage/delta") if params["threadId"] == id => {
                    if let Some(piece) = params["delta"].as_str() {
                        ensure!(
                            text.len() + piece.len() < 4 * 1024 * 1024,
                            "Codex response exceeds the size limit."
                        );
                        text.push_str(piece);
                        delta(piece);
                    }
                }
                Some("item/tool/call") if params["threadId"] == id => {
                    let response_id = value.get("id").context("Missing Codex tool request ID.")?;
                    let name = params["tool"].as_str().unwrap_or_default();
                    let (allowed, output) = if tools.iter().any(|tool| tool.name == name) {
                        call_tool(ToolCall {
                            id: params["callId"].as_str().unwrap_or_default().into(),
                            name: name.into(),
                            arguments: params["arguments"].clone(),
                        })
                        .await
                    } else {
                        (false, "This tool was not exposed by OpenDex.".into())
                    };
                    self.send(json!({"id":response_id,"result":{
                        "success":allowed,"contentItems":[{"type":"inputText","text":output}]
                    }}))
                    .await?;
                }
                Some("error") if params["threadId"] == id => {
                    failure = params["error"]["message"]
                        .as_str()
                        .unwrap_or("The Codex turn failed.")
                        .into();
                }
                Some("turn/completed") if params["threadId"] == id => {
                    let turn = &params["turn"];
                    if turn["status"] != "completed" {
                        bail!(
                            "{}",
                            turn["error"]["message"]
                                .as_str()
                                .unwrap_or(if failure.is_empty() {
                                    "The Codex turn ended before it completed."
                                } else {
                                    &failure
                                })
                        );
                    }
                    ensure!(!text.is_empty(), "Codex returned an empty answer.");
                    return Ok(text);
                }
                Some(_) if value.get("id").is_some() => self.reject(&value["id"]).await?,
                _ => {}
            }
        }
    }
}
