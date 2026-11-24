//! Transport for an installed, unmodified Claude Code executable.
//!
//! Claude Code owns authentication. OpenDex never reads its credential files.
//! This first layer initializes the streaming control protocol without sending
//! a prompt. Native tools, hooks, project settings, and external MCP discovery
//! are disabled so a later tool bridge can apply OpenDex permission decisions.
//! Protocol reference: Anthropic's claude-agent-sdk-python internal/query.py.
use crate::mcp::{bounded_line, process_path};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
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
}

impl Bridge {
    /// `config_home` is dedicated to OpenDex, independent of terminal sessions.
    /// Initialization does not submit a prompt or start a paid inference turn.
    pub async fn start(program: &str, config_home: &Path) -> Result<Self> {
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
                "--mcp-config",
                "{\"mcpServers\":{}}",
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
                    // No native operation is authorized during initialization.
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
}
