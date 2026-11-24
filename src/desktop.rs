//! Tauri commands own persistent state and authorize every model tool request.
//! The webview handles presentation and user input.
use crate::{
    agent::{self, Capability},
    backup, codex,
    mcp::{self, Connection},
    providers::{self, Tool},
    state::{
        Appearance, BranchAction, Data, MAX_RESOURCE_BYTES, MAX_RESOURCES_PER_MESSAGE, McpResource,
        McpScope, Message, Mode, Permissions, Project, Provider, ProviderKind, Role, Rule, Server,
        Store, Thread, ToolCall, Transport, delete_secret, one_or_many, write_secret,
    },
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type Reply<T> = std::result::Result<T, String>;

struct Desktop {
    store: Store,
    client: reqwest::Client,
    codex_home: PathBuf,
    runs: Mutex<HashMap<Uuid, CancellationToken>>,
    workspaces: Mutex<HashSet<PathBuf>>,
    approvals: Mutex<HashMap<Uuid, oneshot::Sender<bool>>>,
    codex_lock: AsyncMutex<()>,
    backup_lock: AsyncMutex<()>,
}

type AppState<'a> = State<'a, Arc<Desktop>>;

fn reply<T>(result: Result<T>) -> Reply<T> {
    result.map_err(|error| format!("{error:#}"))
}

fn fetch_thread(data: &Data, id: Uuid) -> Result<&Thread> {
    data.threads
        .iter()
        .find(|thread| thread.id == id)
        .context("That conversation no longer exists.")
}

fn thread_mut(data: &mut Data, id: Uuid) -> Result<&mut Thread> {
    data.threads
        .iter_mut()
        .find(|thread| thread.id == id)
        .context("That conversation no longer exists.")
}

fn provider(data: &Data, id: Uuid) -> Result<&Provider> {
    data.providers
        .iter()
        .find(|provider| provider.id == id)
        .context("That provider no longer exists.")
}

impl Desktop {
    fn ensure_idle(&self, thread: Uuid) -> Result<()> {
        ensure!(
            !self.runs.lock().expect("runs lock").contains_key(&thread),
            "Stop the current turn first."
        );
        Ok(())
    }
}

#[tauri::command]
fn snapshot(app: AppState<'_>) -> Data {
    app.store.snapshot()
}

#[tauri::command]
fn save_provider(
    app: AppState<'_>,
    mut value: Provider,
    secret: Option<String>,
    forget: bool,
) -> Reply<Provider> {
    reply(app.store.change(|data| {
        value.validate()?;
        if value.kind == ProviderKind::ChatGptAccount {
            ensure!(
                !data
                    .providers
                    .iter()
                    .any(|item| item.kind == value.kind && item.id != value.id),
                "The ChatGPT connection is already configured."
            );
        }
        let existing = data
            .providers
            .iter()
            .find(|provider| provider.id == value.id);
        value.secret_ref = existing.and_then(|old| old.secret_ref);
        let changed =
            existing.is_some_and(|old| old.endpoint != value.endpoint || old.kind != value.kind);
        value.has_secret = existing.is_some_and(|old| old.has_secret) && !changed;
        if let Some(old) = existing.filter(|old| old.has_secret && (forget || changed)) {
            delete_secret(old.credential_id(), &old.endpoint)?;
            value.has_secret = false;
        }
        if let Some(secret) = &secret {
            ensure!(
                value.kind != ProviderKind::ChatGptAccount,
                "Connect this provider through its account flow."
            );
            write_secret(value.credential_id(), &value.endpoint, secret)?;
            value.has_secret = true;
        }
        if let Some(item) = data
            .providers
            .iter_mut()
            .find(|provider| provider.id == value.id)
        {
            *item = value.clone();
        } else {
            data.providers.push(value.clone());
        }
        Ok(value)
    }))
}

#[tauri::command]
fn remove_provider(app: AppState<'_>, id: Uuid) -> Reply<()> {
    reply(app.store.change(|data| {
        let old = provider(data, id)?;
        if old.has_secret {
            delete_secret(old.credential_id(), &old.endpoint)?;
        }
        data.providers.retain(|value| value.id != id);
        for thread in &mut data.threads {
            if thread.provider_id == Some(id) {
                thread.provider_id = None;
                thread.model_id = None;
            }
        }
        Ok(())
    }))
}

#[tauri::command]
async fn discover_models(app: AppState<'_>, id: Uuid) -> Reply<Vec<String>> {
    reply(
        async {
            let data = app.store.snapshot();
            let provider = provider(&data, id)?;
            if provider.kind == ProviderKind::ChatGptAccount {
                let _lock = app
                    .codex_lock
                    .try_lock()
                    .context("The ChatGPT connection is busy.")?;
                let mut bridge = codex::Bridge::start(&data.codex_path, &app.codex_home).await?;
                return bridge.models().await;
            }
            providers::discover(&app.client, provider).await
        }
        .await,
    )
}

#[tauri::command]
fn save_server(
    app: AppState<'_>,
    mut value: Server,
    secret: Option<String>,
    forget: bool,
) -> Reply<Server> {
    reply(app.store.change(|data| {
        value.validate()?;
        let existing = data.servers.iter().find(|server| server.id == value.id);
        value.secret_ref = existing.and_then(|old| old.secret_ref);
        let changed = existing
            .is_some_and(|old| old.transport != value.transport || old.address != value.address);
        value.has_secret = existing.is_some_and(|old| old.has_secret) && !changed;
        if let Some(old) = existing.filter(|old| old.has_secret && (forget || changed)) {
            delete_secret(old.credential_id(), &old.address)?;
            value.has_secret = false;
        }
        if let Some(secret) = &secret {
            ensure!(
                value.transport == Transport::Http,
                "Bearer keys are supported for HTTP servers."
            );
            write_secret(value.credential_id(), &value.address, secret)?;
            value.has_secret = true;
        }
        if let Some(item) = data.servers.iter_mut().find(|server| server.id == value.id) {
            *item = value.clone();
        } else {
            data.servers.push(value.clone());
        }
        Ok(value)
    }))
}

#[tauri::command]
fn remove_server(app: AppState<'_>, id: Uuid) -> Reply<()> {
    reply(app.store.change(|data| {
        let server = data
            .servers
            .iter()
            .find(|server| server.id == id)
            .context("That MCP server no longer exists.")?;
        if server.has_secret {
            delete_secret(server.credential_id(), &server.address)?;
        }
        data.servers.retain(|server| server.id != id);
        for thread in &mut data.threads {
            if let McpScope::Selected(servers) = &mut thread.mcp_scope {
                servers.retain(|server| *server != id);
            }
        }
        Ok(())
    }))
}

#[derive(Serialize)]
struct ServerStatus {
    version: String,
    tools: Vec<Tool>,
}

#[tauri::command]
async fn test_server(app: AppState<'_>, id: Uuid) -> Reply<ServerStatus> {
    reply(
        async {
            let data = app.store.snapshot();
            let server = data
                .servers
                .iter()
                .find(|server| server.id == id)
                .context("Save the server before testing it.")?;
            let mut session = Connection::connect(&app.client, server, None).await?;
            let result = session.tools().await.map(|tools| ServerStatus {
                version: session.version.clone(),
                tools,
            });
            session.close().await;
            result
        }
        .await,
    )
}

fn enabled_mcp_server<'a>(data: &'a Data, thread: &Thread, id: Uuid) -> Result<&'a Server> {
    ensure!(
        data.permissions.effective().mcp != Rule::Deny,
        "This permission profile disables MCP."
    );
    ensure!(
        thread.mcp_scope.includes(id),
        "This MCP server is unavailable in the current conversation."
    );
    data.servers
        .iter()
        .find(|server| server.id == id && server.enabled)
        .context("Enable this MCP server in Settings first.")
}

async fn connect_context(app: &Desktop, thread: Uuid, id: Uuid) -> Result<Connection> {
    app.ensure_idle(thread)?;
    let data = app.store.snapshot();
    let thread = fetch_thread(&data, thread)?;
    let server = enabled_mcp_server(&data, thread, id)?;
    let root = if server.transport == Transport::Stdio {
        thread
            .workspace
            .as_deref()
            .map(agent::checked_root)
            .transpose()?
    } else {
        None
    };
    Connection::connect(&app.client, server, root.as_deref()).await
}

#[tauri::command]
async fn mcp_context_catalog(
    app: AppState<'_>,
    thread: Uuid,
    id: Uuid,
) -> Reply<mcp::ContextCatalog> {
    reply(
        async {
            let mut session = connect_context(&app, thread, id).await?;
            let result = session.context_catalog().await;
            session.close().await;
            result
        }
        .await,
    )
}

#[tauri::command]
async fn mcp_read_resource(
    app: AppState<'_>,
    thread: Uuid,
    id: Uuid,
    uri: String,
) -> Reply<mcp::ResourcePreview> {
    reply(
        async {
            let mut session = connect_context(&app, thread, id).await?;
            let result = session.read_resource(&uri).await;
            session.close().await;
            result
        }
        .await,
    )
}

#[tauri::command]
async fn mcp_get_prompt(
    app: AppState<'_>,
    thread: Uuid,
    id: Uuid,
    name: String,
    arguments: BTreeMap<String, String>,
) -> Reply<Vec<mcp::PromptMessage>> {
    reply(
        async {
            let mut session = connect_context(&app, thread, id).await?;
            let result = session.get_prompt(&name, arguments).await;
            session.close().await;
            result
        }
        .await,
    )
}

#[tauri::command]
fn save_settings(
    app: AppState<'_>,
    appearance: Appearance,
    permissions: Permissions,
    codex_path: String,
    instructions: String,
) -> Reply<()> {
    reply(
        app.store
            .update_settings(appearance, permissions, codex_path, instructions),
    )
}

#[tauri::command]
async fn export_settings_backup(app: AppState<'_>, path: PathBuf, password: String) -> Reply<()> {
    reply(
        async {
            let _lock = app
                .backup_lock
                .try_lock()
                .context("A backup operation is already in progress.")?;
            let app = Arc::clone(&app);
            tauri::async_runtime::spawn_blocking(move || {
                backup::export(&app.store, &path, password)
            })
            .await?
        }
        .await,
    )
}

#[tauri::command]
async fn preview_settings_backup(
    app: AppState<'_>,
    path: PathBuf,
    password: String,
) -> Reply<backup::Preview> {
    reply(
        async {
            let _lock = app
                .backup_lock
                .try_lock()
                .context("A backup operation is already in progress.")?;
            tauri::async_runtime::spawn_blocking(move || {
                let (archive, checksum) = backup::open(&path, password, None)?;
                Ok(archive.preview(checksum))
            })
            .await?
        }
        .await,
    )
}

#[tauri::command]
async fn restore_settings_backup(
    app: AppState<'_>,
    path: PathBuf,
    password: String,
    checksum: [u8; 32],
    project_folders: HashMap<Uuid, PathBuf>,
) -> Reply<backup::RestoreResult> {
    reply(
        async {
            let _lock = app
                .backup_lock
                .try_lock()
                .context("A backup operation is already in progress.")?;
            let app = Arc::clone(&app);
            tauri::async_runtime::spawn_blocking(move || {
                let (archive, _) = backup::open(&path, password, Some(checksum))?;
                let runs = app.runs.lock().expect("runs lock");
                ensure!(
                    runs.is_empty(),
                    "Finish or stop active turns before restoring settings."
                );
                archive.restore(&app.store, project_folders)
            })
            .await?
        }
        .await,
    )
}

#[tauri::command]
async fn account_action(
    app: AppState<'_>,
    window: AppHandle,
    action: String,
) -> Reply<codex::Account> {
    reply(
        async {
            let _lock = app
                .codex_lock
                .try_lock()
                .context("The ChatGPT connection is busy.")?;
            let data = app.store.snapshot();
            let mut bridge = codex::Bridge::start(&data.codex_path, &app.codex_home).await?;
            match action.as_str() {
                "status" => bridge.account().await,
                "connect" => {
                    bridge
                        .login(|url| {
                            if let Err(error) = open_browser(url) {
                                let _ = window.emit(
                                    "opendex-connection",
                                    json!({"url":url,"error":error.to_string()}),
                                );
                            } else {
                                let _ = window.emit("opendex-connection", json!({"url":url}));
                            }
                        })
                        .await
                }
                "disconnect" => {
                    bridge.logout().await?;
                    bridge.account().await
                }
                _ => bail!("Unknown ChatGPT account action."),
            }
        }
        .await,
    )
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut child = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut child = {
        let mut child = std::process::Command::new("rundll32.exe");
        child.arg("url.dll,FileProtocolHandler");
        child
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut child = std::process::Command::new("xdg-open");
    ensure!(
        child.arg(url).status()?.success(),
        "Could not open the system browser."
    );
    Ok(())
}

fn insert_thread(data: &mut Data, mode: Mode, project: Option<Project>) -> Thread {
    let previous = data.threads.iter().rev().find(|thread| thread.mode == mode);
    let provider = previous
        .and_then(|thread| thread.provider_id)
        .and_then(|id| provider(data, id).ok())
        .or_else(|| {
            data.providers
                .iter()
                .find(|provider| !provider.models.is_empty())
        })
        .or(data.providers.first());
    let model_id = provider.and_then(|provider| {
        previous
            .and_then(|thread| thread.model_id.as_ref())
            .filter(|model| provider.models.contains(model))
            .or_else(|| provider.models.first())
            .cloned()
    });
    let thread = Thread {
        id: Uuid::new_v4(),
        mode,
        title: "New conversation".into(),
        messages: vec![],
        workspace: project.as_ref().map(|project| project.root.clone()),
        project_id: project.map(|project| project.id),
        provider_id: provider.map(|provider| provider.id),
        model_id,
        instructions: String::new(),
        mcp_scope: McpScope::default(),
        parent_thread: None,
    };
    data.active_thread = Some(thread.id);
    data.threads.push(thread.clone());
    thread
}

fn find_project(data: &Data, id: Uuid) -> Result<Project> {
    data.projects
        .iter()
        .find(|project| project.id == id)
        .cloned()
        .context("That project no longer exists.")
}

#[tauri::command]
fn new_thread(app: AppState<'_>, mode: Mode, project: Option<Uuid>) -> Reply<Thread> {
    reply(app.store.change(|data| {
        ensure!(
            mode == Mode::Agent || project.is_none(),
            "Model Chat does not use project folders."
        );
        let project = project.map(|id| find_project(data, id)).transpose()?;
        Ok(insert_thread(data, mode, project))
    }))
}

#[tauri::command]
fn open_project(app: AppState<'_>, path: PathBuf) -> Reply<Thread> {
    reply((|| {
        let root = agent::checked_root(&path)?;
        app.store.change(|data| {
            let project = data.project_for_root(root);
            if let Some(thread) = data
                .threads
                .iter()
                .rev()
                .find(|thread| thread.project_id == Some(project.id))
                .cloned()
            {
                data.active_thread = Some(thread.id);
                return Ok(thread);
            }
            Ok(insert_thread(data, Mode::Agent, Some(project)))
        })
    })())
}

#[derive(Deserialize)]
struct ThreadChanges {
    title: Option<String>,
    selection: Option<Selection>,
    workspace: Option<PathBuf>,
    project: Option<Uuid>,
    clear_workspace: Option<bool>,
    instructions: Option<String>,
    mcp_scope: Option<McpScope>,
}

#[derive(Deserialize)]
struct Selection {
    provider: Uuid,
    model: Option<String>,
}

#[tauri::command]
fn edit_thread(app: AppState<'_>, id: Uuid, changes: ThreadChanges) -> Reply<Thread> {
    reply((|| {
        let workspace = changes
            .workspace
            .as_deref()
            .map(agent::checked_root)
            .transpose()?;
        // Keep the edit atomic with respect to a turn taking its settings snapshot.
        let runs = app.runs.lock().expect("runs lock");
        ensure!(
            !runs.contains_key(&id),
            "This conversation already has a turn in progress."
        );
        app.store.change(|data| {
            if let Some(selection) = &changes.selection {
                let provider = provider(data, selection.provider)?;
                if let Some(model) = &selection.model {
                    ensure!(
                        provider.models.contains(model),
                        "Add the model to this provider's list first."
                    );
                }
            }
            if let Some(McpScope::Selected(servers)) = &changes.mcp_scope {
                ensure!(
                    servers
                        .iter()
                        .all(|id| data.servers.iter().any(|server| server.id == *id)),
                    "An MCP server in the selection no longer exists."
                );
            }
            let has_project_change = workspace.is_some()
                || changes.project.is_some()
                || changes.clear_workspace == Some(true);
            if has_project_change {
                ensure!(
                    fetch_thread(data, id)?.mode == Mode::Agent,
                    "Only Code Agent conversations use project folders."
                );
            }
            let project = if let Some(workspace) = workspace {
                Some(data.project_for_root(workspace))
            } else {
                changes
                    .project
                    .map(|id| find_project(data, id))
                    .transpose()?
            };
            let thread = thread_mut(data, id)?;
            if let Some(title) = changes.title {
                ensure!(!title.trim().is_empty(), "The title is empty.");
                thread.title = title.trim().chars().take(130).collect();
            }
            if let Some(selection) = changes.selection {
                thread.provider_id = Some(selection.provider);
                thread.model_id = selection.model;
            }
            if let Some(instructions) = changes.instructions {
                thread.instructions = instructions;
            }
            if let Some(scope) = changes.mcp_scope {
                thread.mcp_scope = scope;
            }
            if has_project_change {
                thread.project_id = project.as_ref().map(|project| project.id);
                thread.workspace = project.map(|project| project.root);
            }
            Ok(thread.clone())
        })
    })())
}

#[tauri::command]
fn branch_thread(
    app: AppState<'_>,
    id: Uuid,
    message: Uuid,
    action: BranchAction,
) -> Reply<Thread> {
    reply(app.store.change(|data| {
        let branch = fetch_thread(data, id)?.branch_at(message, action)?;
        data.active_thread = Some(branch.id);
        data.threads.push(branch.clone());
        Ok(branch)
    }))
}

#[tauri::command]
fn set_active(app: AppState<'_>, id: Uuid) -> Reply<()> {
    reply(app.store.change(|data| {
        fetch_thread(data, id)?;
        data.active_thread = Some(id);
        Ok(())
    }))
}

#[tauri::command]
fn remove_thread(app: AppState<'_>, id: Uuid) -> Reply<()> {
    reply((|| {
        app.ensure_idle(id)?;
        app.store.change(|data| {
            data.threads.retain(|thread| thread.id != id);
            if data.active_thread == Some(id) {
                data.active_thread = data.threads.last().map(|thread| thread.id);
            }
            Ok(())
        })
    })())
}

#[derive(Serialize)]
struct AttachmentPreview {
    path: PathBuf,
    name: String,
}

#[tauri::command]
async fn inspect_attachments(paths: Vec<PathBuf>) -> Reply<Vec<AttachmentPreview>> {
    reply(
        async {
            tauri::async_runtime::spawn_blocking(move || {
                Ok(agent::load_attachments(paths)?
                    .into_iter()
                    .map(|(path, file)| AttachmentPreview {
                        path,
                        name: file.name,
                    })
                    .collect())
            })
            .await?
        }
        .await,
    )
}

#[tauri::command]
fn export_thread(app: AppState<'_>, id: Uuid, path: PathBuf) -> Reply<()> {
    reply((|| {
        let data = app.store.snapshot();
        let thread = fetch_thread(&data, id)?;
        let mut content = format!("# {}\n", thread.title);
        for message in &thread.messages {
            if message.role == Role::Tool || !message.has_content() {
                continue;
            }
            let label = if message.role == Role::User {
                "You"
            } else {
                message.model.as_deref().unwrap_or("Assistant")
            };
            content.push_str(&format!("\n## {label}\n\n{}\n", message.content()));
        }
        std::fs::write(path, content)?;
        Ok(())
    })())
}

#[tauri::command]
async fn review_changes(app: AppState<'_>, id: Uuid) -> Reply<String> {
    reply(
        async {
            let data = app.store.snapshot();
            let workspace = fetch_thread(&data, id)?
                .workspace
                .as_deref()
                .context("Choose a project folder.")?;
            agent::diff(&agent::checked_root(workspace)?).await
        }
        .await,
    )
}

#[tauri::command]
async fn add_worktree(
    app: AppState<'_>,
    id: Uuid,
    target: PathBuf,
    branch: String,
) -> Reply<Thread> {
    reply(
        async {
            app.ensure_idle(id)?;
            let data = app.store.snapshot();
            let workspace = fetch_thread(&data, id)?
                .workspace
                .as_deref()
                .context("Choose a Git project folder first.")?;
            let workspace = agent::checked_root(workspace)?;
            let output = agent::create_worktree(&workspace, &target, branch.trim()).await?;
            ensure!(output.starts_with("Exit 0\n"), "{output}");
            let root = agent::checked_root(&target)?;
            app.store.change(|data| {
                let thread = thread_mut(data, id)?;
                thread.workspace = Some(root);
                Ok(thread.clone())
            })
        }
        .await,
    )
}

struct RunGuard {
    state: Arc<Desktop>,
    thread: Uuid,
    root: Option<PathBuf>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.state
            .runs
            .lock()
            .expect("runs lock")
            .remove(&self.thread);
        if let Some(root) = &self.root {
            self.state
                .workspaces
                .lock()
                .expect("workspaces lock")
                .remove(root);
        }
    }
}

struct ApprovalGuard<'a> {
    state: &'a Desktop,
    id: Uuid,
}
impl Drop for ApprovalGuard<'_> {
    fn drop(&mut self) {
        self.state
            .approvals
            .lock()
            .expect("approvals lock")
            .remove(&self.id);
    }
}

#[derive(Clone)]
struct Turn {
    app: Arc<Desktop>,
    window: AppHandle,
    id: Uuid,
    root: Option<PathBuf>,
    rules: Permissions,
    partial: Arc<Mutex<String>>,
}

struct ToolSession {
    turn: Turn,
    allowed: HashSet<String>,
    remote: HashMap<String, (usize, String)>,
    sessions: Vec<Connection>,
    used: usize,
}

impl Turn {
    fn event(&self, kind: &str, details: Value) {
        let _ = self.window.emit(
            "opendex-turn",
            json!({"thread":self.id,"kind":kind,"details":details}),
        );
    }

    fn delta(&self, piece: &str) {
        self.partial.lock().expect("stream buffer").push_str(piece);
        self.event("delta", json!({"text":piece}));
    }

    fn checkpoint(&self, mut message: Message) -> Result<()> {
        message.output_items.clear();
        self.append(message.clone())?;
        self.partial.lock().expect("stream buffer").clear();
        self.event("checkpoint", json!({"message":message}));
        Ok(())
    }

    fn save_partial(&self, model: String, provider: Uuid) -> Result<()> {
        let text = std::mem::take(&mut *self.partial.lock().expect("stream buffer"));
        if !text.is_empty() {
            let mut message = Message::new(Role::Assistant, text);
            message.model = Some(model);
            message.provider = Some(provider);
            message.incomplete = true;
            self.append(message)?;
        }
        Ok(())
    }

    fn append(&self, message: Message) -> Result<()> {
        self.app.store.change(|data| {
            thread_mut(data, self.id)?.messages.push(message);
            Ok(())
        })
    }

    async fn approve(&self, name: &str, arguments: &Value, source: &str, rule: Rule) -> bool {
        match rule {
            Rule::Deny => false,
            Rule::Allow => true,
            Rule::Ask => {
                let id = Uuid::new_v4();
                let (send, receive) = oneshot::channel();
                self.app
                    .approvals
                    .lock()
                    .expect("approvals lock")
                    .insert(id, send);
                let _guard = ApprovalGuard {
                    state: &self.app,
                    id,
                };
                self.event(
                    "approval",
                    json!({"id":id,"tool":name,"arguments":arguments,"source":source}),
                );
                let decision = tokio::time::timeout(Duration::from_secs(300), receive)
                    .await
                    .ok()
                    .and_then(std::result::Result::ok)
                    .unwrap_or(false);
                self.event("resolved", json!({"id":id}));
                decision
            }
        }
    }
}

impl ToolSession {
    async fn start(turn: Turn, data: &Data, thread: &Thread) -> (Self, Vec<Tool>) {
        let mut tools = agent::tools(thread.mode, turn.root.as_deref(), &turn.rules);
        let mut session = Self {
            turn,
            allowed: HashSet::new(),
            remote: HashMap::new(),
            sessions: vec![],
            used: 0,
        };
        let global = session.turn.rules.mcp;
        if global != Rule::Deny {
            for server in data
                .servers
                .iter()
                .filter(|server| {
                    server.enabled
                        && thread.mcp_scope.includes(server.id)
                        && server.server_tool_rule(global) != Rule::Deny
                })
                .take(20)
            {
                let connect = async {
                    let mut connection = mcp::Connection::connect(
                        &session.turn.app.client,
                        server,
                        session.turn.root.as_deref(),
                    )
                    .await?;
                    let tools = connection.tools().await?;
                    Ok::<_, anyhow::Error>((connection, tools))
                }
                .await;
                match connect {
                    Ok((connection, catalog)) => {
                        let position = session.sessions.len();
                        session.sessions.push(connection);
                        for mut tool in catalog
                            .into_iter()
                            .filter(|tool| server.tool_rule(global, &tool.name) != Rule::Deny)
                        {
                            let suffix: String = tool
                                .name
                                .chars()
                                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                                .take(32)
                                .collect();
                            let alias = format!("mcp_{}_{}_{}", position, tools.len(), suffix);
                            tool.description =
                                format!("{} · {}\n{}", server.name, tool.name, tool.description);
                            session.remote.insert(alias.clone(), (position, tool.name));
                            tool.name = alias;
                            tools.push(tool);
                            if tools.len() == 96 {
                                break;
                            }
                        }
                    }
                    Err(error) => session.turn.event(
                        "notice",
                        json!({"message":format!("{}: {error:#}",server.name)}),
                    ),
                }
                if tools.len() == 96 {
                    break;
                }
            }
        }
        session
            .allowed
            .extend(tools.iter().map(|tool| tool.name.clone()));
        (session, tools)
    }

    fn tool_label(&self, name: &str) -> String {
        if let Some((position, tool)) = self.remote.get(name) {
            return format!("{} · {tool}", self.sessions[*position].server.name);
        }
        match name {
            "list_files" => "List files",
            "read_file" => "Read file",
            "search_files" => "Search files",
            "workspace_diff" => "Review changes",
            "create_file" => "Create file",
            "edit_file" => "Edit file",
            "run_command" => "Run command",
            _ => "Tool result",
        }
        .into()
    }

    async fn call(&mut self, call: &ToolCall) -> (bool, String) {
        self.used += 1;
        if self.used > self.turn.rules.max_steps.into() {
            return (
                false,
                "The tool-call limit was reached. Summarize the work so far.".into(),
            );
        }
        let result = self.authorize_and_call(call).await;
        let allowed = result.is_ok();
        let output = result.unwrap_or_else(|error| format!("Tool error: {error:#}"));
        self.turn.event(
            "tool",
            json!({"name":self.tool_label(&call.name),"output":output,"success":allowed}),
        );
        (allowed, output)
    }

    async fn authorize_and_call(&mut self, call: &ToolCall) -> Result<String> {
        ensure!(
            self.allowed.contains(&call.name),
            "This tool was not made available."
        );
        ensure!(
            call.arguments.is_object() && call.arguments.get("_invalid_arguments").is_none(),
            "The tool arguments must be a JSON object."
        );
        if let Some((position, name)) = self.remote.get(&call.name) {
            let server = &mut self.sessions[*position];
            ensure!(
                self.turn
                    .approve(
                        name,
                        &call.arguments,
                        &server.server.name,
                        server.server.tool_rule(self.turn.rules.mcp, name)
                    )
                    .await,
                "MCP call declined."
            );
            return server.call(name, call.arguments.clone()).await;
        }
        let cap = agent::capability(&call.name).context("Unknown tool.")?;
        let rule = cap.rule(&self.turn.rules);
        ensure!(
            self.turn
                .approve(&call.name, &call.arguments, "Workspace", rule)
                .await,
            "Workspace tool declined."
        );
        let root = self
            .turn
            .root
            .as_deref()
            .context("Choose a project folder.")?;
        let mut policy = self.turn.rules.clone();
        // Automatic shell execution must not inherit permissions that would
        // have triggered their own prompt when using a workspace file tool.
        if matches!(cap, Capability::Command) && rule == Rule::Allow {
            if policy.write == Rule::Ask {
                policy.write = Rule::Deny;
            }
            if policy.read == Rule::Ask {
                policy.read = Rule::Deny;
            }
        }
        agent::execute(root, &policy, &call.name, &call.arguments).await
    }

    async fn close(self) {
        for session in self.sessions {
            session.close().await;
        }
    }
}

async fn complete_turn(
    turn: Turn,
    data: Data,
    thread: Thread,
    provider: Provider,
    model: String,
) -> Result<()> {
    let (mut session, tools) = ToolSession::start(turn.clone(), &data, &thread).await;
    let mut system = match thread.mode {
        Mode::Chat => "You are a helpful assistant in OpenDex Model Chat. Use permitted tools when helpful.".to_owned(),
        Mode::Agent => "You are OpenDex Code Agent. Inspect relevant project files before editing. Use only the tools available to you. Respect permission decisions. Test your changes when possible, and report what you verified.".to_owned(),
    };
    if thread.mode == Mode::Agent && turn.root.is_none() {
        system.push_str(" A project folder has not been selected. Provide guidance without assuming access to local files.");
    }
    for instructions in [&data.instructions, &thread.instructions] {
        if !instructions.trim().is_empty() {
            system.push_str("\n\n");
            system.push_str(instructions);
        }
    }

    // Keep tool exchange wire formats local to the turn. Plain conversation
    // history stays portable across providers and models.
    let mut messages: Vec<_> = thread
        .messages
        .into_iter()
        .filter_map(|mut message| {
            if message.incomplete || message.role == Role::Tool || !message.has_content() {
                return None;
            }
            message.calls.clear();
            message.output_items.clear();
            Some(message)
        })
        .collect();
    let result = if provider.kind == ProviderKind::ChatGptAccount {
        let _lock = turn
            .app
            .codex_lock
            .try_lock()
            .context("The ChatGPT connection is busy.")?;
        let mut bridge = codex::Bridge::start(&data.codex_path, &turn.app.codex_home).await?;
        let session_lock = AsyncMutex::new(&mut session);
        let text = bridge
            .generate(
                &model,
                &system,
                &messages,
                &tools,
                |piece| {
                    turn.delta(piece);
                },
                |call| {
                    let lock = &session_lock;
                    let context = &turn;
                    async move {
                        let mut session = lock.lock().await;
                        let label = session.tool_label(&call.name);
                        let (allowed, output) = session.call(&call).await;
                        let mut message = Message::new(Role::Tool, &output);
                        message.tool_label = Some(label);
                        message.tool_name = Some(call.name);
                        message.tool_id = Some(call.id);
                        match context.append(message) {
                            Ok(()) => (allowed, output),
                            Err(error) => {
                                (false, format!("Could not save the tool result: {error:#}"))
                            }
                        }
                    }
                },
            )
            .await?;
        let mut message = Message::new(Role::Assistant, text);
        message.model = Some(model);
        message.provider = Some(provider.id);
        turn.checkpoint(message)
    } else {
        async {
            for step in 0..=turn.rules.max_steps {
                let available = if session.used < usize::from(turn.rules.max_steps)
                    && step < turn.rules.max_steps
                {
                    &tools[..]
                } else {
                    &[]
                };
                let answer = providers::generate(
                    &turn.app.client,
                    &provider,
                    &model,
                    &system,
                    &messages,
                    available,
                    |piece| {
                        turn.delta(piece);
                    },
                )
                .await?;
                let mut message = Message::new(Role::Assistant, answer.text);
                message.model = Some(model.clone());
                message.provider = Some(provider.id);
                message.calls = answer.calls;
                message.output_items = answer.output_items;
                turn.checkpoint(message.clone())?;
                if message.calls.is_empty() {
                    return Ok(());
                }
                ensure!(
                    !available.is_empty(),
                    "The model kept requesting tools after the tool budget was exhausted."
                );
                let calls = message.calls.clone();
                messages.push(message);
                for call in calls {
                    let (_, output) = session.call(&call).await;
                    let mut transcript = Message::new(Role::Tool, output);
                    transcript.tool_label = Some(session.tool_label(&call.name));
                    transcript.tool_id = Some(call.id);
                    transcript.tool_name = Some(call.name);
                    turn.append(transcript.clone())?;
                    messages.push(transcript);
                }
            }
            bail!("The agent reached its tool budget.")
        }
        .await
    };
    session.close().await;
    result
}

#[derive(Deserialize)]
struct Prompt {
    thread: Uuid,
    text: String,
    #[serde(default, alias = "attachment", deserialize_with = "one_or_many")]
    attachments: Vec<PathBuf>,
    #[serde(default)]
    resources: Vec<McpResource>,
}

fn validate_resources(data: &Data, thread: &Thread, resources: &[McpResource]) -> Result<()> {
    ensure!(
        resources.len() <= MAX_RESOURCES_PER_MESSAGE,
        "Attach up to eight MCP resources per message."
    );
    let mut size = 0;
    for resource in resources {
        enabled_mcp_server(data, thread, resource.server_id)?;
        ensure!(
            !resource.name.trim().is_empty()
                && resource.name.len() <= 512
                && resource.source.len() <= 512
                && resource.uri.len() <= 2048
                && !resource.text.trim().is_empty(),
            "The MCP resource attachment is invalid."
        );
        size += resource.text.len();
        ensure!(
            size <= MAX_RESOURCE_BYTES,
            "These MCP resources exceed the combined text size limit."
        );
    }
    Ok(())
}

enum TurnInput {
    New(Prompt),
    Existing(Uuid),
}

#[tauri::command]
async fn send_message(app: AppState<'_>, window: AppHandle, prompt: Prompt) -> Reply<()> {
    reply(run_user_turn(&app, window, prompt.thread, TurnInput::New(prompt)).await)
}

#[tauri::command]
async fn reply_in_branch(
    app: AppState<'_>,
    window: AppHandle,
    id: Uuid,
    message: Uuid,
) -> Reply<()> {
    reply(run_user_turn(&app, window, id, TurnInput::Existing(message)).await)
}

async fn run_user_turn(
    app: &Arc<Desktop>,
    window: AppHandle,
    id: Uuid,
    input: TurnInput,
) -> Result<()> {
    let (data, provider, model, root, cancel, _guard) = {
        let mut runs = app.runs.lock().expect("runs lock");
        ensure!(
            !runs.contains_key(&id),
            "This conversation already has a turn in progress."
        );
        let data = app.store.snapshot();
        let thread = fetch_thread(&data, id)?;
        let provider = provider(&data, thread.provider_id.context("Choose a provider.")?)?.clone();
        let model = thread
            .model_id
            .clone()
            .filter(|model| provider.models.contains(model))
            .context("Choose a model from this provider.")?;
        let root = thread
            .workspace
            .as_deref()
            .map(agent::checked_root)
            .transpose()?;
        if let Some(root) = &root {
            let mut workspaces = app.workspaces.lock().expect("workspaces lock");
            ensure!(
                workspaces.insert(root.clone()),
                "Another agent is using this project folder."
            );
        }
        let cancel = CancellationToken::new();
        runs.insert(id, cancel.clone());
        let guard = RunGuard {
            state: Arc::clone(app),
            thread: id,
            root: root.clone(),
        };
        (data, provider, model, root, cancel, guard)
    };
    let thread = fetch_thread(&data, id)?;
    let new_message = matches!(&input, TurnInput::New(_));
    let message = match input {
        TurnInput::New(prompt) => {
            validate_resources(&data, thread, &prompt.resources)?;
            let mut message = Message::new(Role::User, prompt.text.trim());
            if !prompt.attachments.is_empty() {
                message.attachments = tauri::async_runtime::spawn_blocking(move || {
                    agent::load_attachments(prompt.attachments)
                })
                .await??
                .into_iter()
                .map(|(_, file)| file)
                .collect();
            }
            message.resources = prompt.resources;
            message
        }
        TurnInput::Existing(message) => {
            ensure!(
                thread.mode == Mode::Chat && thread.parent_thread.is_some(),
                "Retry is available in a Model Chat branch."
            );
            thread
                .messages
                .last()
                .filter(|last| last.id == message && last.role == Role::User)
                .cloned()
                .context("This branch has already moved on from that message.")?
        }
    };
    message.validate_user_content()?;
    if matches!(
        provider.kind,
        ProviderKind::Ollama | ProviderKind::OllamaCloud | ProviderKind::ChatGptAccount
    ) && let Some((file, _)) = message
        .media_attachments()
        .find(|(_, mime)| *mime == "application/pdf")
    {
        bail!("Choose a PDF-capable API provider for {}.", file.name);
    }
    let thread = if new_message {
        app.store.change(|data| {
            let thread = thread_mut(data, id)?;
            if thread.messages.is_empty() && thread.title == "New conversation" {
                let title = message
                    .text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .or_else(|| message.attachments.first().map(|file| file.name.as_str()))
                    .or_else(|| {
                        message
                            .resources
                            .first()
                            .map(|resource| resource.name.as_str())
                    })
                    .unwrap_or("New conversation")
                    .trim();
                thread.title = title.chars().take(58).collect();
                if title.chars().count() > 58 {
                    thread.title.push('…');
                }
            }
            thread.messages.push(message.clone());
            Ok(thread.clone())
        })?
    } else {
        // The prompt is already stored in the branch, including attachments.
        thread.clone()
    };
    let turn = Turn {
        app: Arc::clone(app),
        window,
        id,
        root,
        rules: data.permissions.effective(),
        partial: Arc::new(Mutex::new(String::new())),
    };
    turn.event("accepted", json!({"message":message,"title":thread.title}));
    let provider_id = provider.id;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            turn.event("stopped", json!({}));
            Ok(())
        }
        result = complete_turn(turn.clone(), data, thread, provider, model.clone()) => result,
    };
    turn.save_partial(model, provider_id)?;
    result
}

#[tauri::command]
fn stop_turn(app: AppState<'_>, id: Uuid) -> Reply<()> {
    if let Some(cancel) = app.runs.lock().expect("runs lock").get(&id) {
        cancel.cancel();
    }
    Ok(())
}

#[tauri::command]
fn resolve_approval(app: AppState<'_>, id: Uuid, allow: bool) -> Reply<()> {
    if let Some(sender) = app.approvals.lock().expect("approvals lock").remove(&id) {
        let _ = sender.send(allow);
    }
    Ok(())
}

// The app lock is independent of --data-dir. Keep its file handle open
// through application shutdown so the OS releases it on either quit or crash.
fn acquire_instance(identifier: &str) -> Result<Option<fs::File>> {
    let base =
        directories::BaseDirs::new().context("Could not find the local app data directory.")?;
    let directory = base.data_local_dir().join(identifier);
    fs::create_dir_all(&directory)?;
    let lock = crate::state::open_lock_file(&directory.join("application.lock"))?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => {
            Err(error).context("Could not lock the OpenDex application.")
        }
    }
}

pub fn run() {
    let context = tauri::generate_context!();
    let _instance = match acquire_instance(&context.config().identifier) {
        Ok(Some(lock)) => lock,
        Ok(None) => return,
        Err(error) => {
            eprintln!("OpenDex could not start: {error:#}");
            return;
        }
    };
    let directory = std::env::args_os().skip(1).collect::<Vec<_>>();
    let override_home = directory
        .windows(2)
        .find(|args| args[0] == "--data-dir")
        .map(|args| PathBuf::from(&args[1]))
        .or_else(|| std::env::var_os("OPENDEX_DATA_DIR").map(PathBuf::from));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let home = if let Some(path) = override_home {
                path
            } else {
                app.path().app_data_dir()?
            };
            app.manage(Arc::new(Desktop {
                store: Store::open(&home)?,
                client: providers::client()?,
                codex_home: home.join("codex"),
                runs: Mutex::new(HashMap::new()),
                workspaces: Mutex::new(HashSet::new()),
                approvals: Mutex::new(HashMap::new()),
                codex_lock: AsyncMutex::new(()),
                backup_lock: AsyncMutex::new(()),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_provider,
            remove_provider,
            discover_models,
            save_server,
            remove_server,
            test_server,
            mcp_context_catalog,
            mcp_read_resource,
            mcp_get_prompt,
            save_settings,
            export_settings_backup,
            preview_settings_backup,
            restore_settings_backup,
            account_action,
            new_thread,
            open_project,
            edit_thread,
            branch_thread,
            set_active,
            remove_thread,
            inspect_attachments,
            export_thread,
            review_changes,
            add_worktree,
            send_message,
            reply_in_branch,
            stop_turn,
            resolve_approval
        ])
        .run(context)
        .expect("could not start OpenDex");
}
