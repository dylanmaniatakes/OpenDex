//! Persistent app data. Credentials stay in the system credential store.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    OpenAiCompatible,
    Anthropic,
    Ollama,
    OllamaCloud,
    ChatGptAccount,
}

impl ProviderKind {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::OpenAiCompatible => "http://127.0.0.1:1234/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Ollama => "http://127.0.0.1:11434/api",
            Self::OllamaCloud => "https://ollama.com/api",
            Self::ChatGptAccount => "",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: Uuid,
    pub name: String,
    pub kind: ProviderKind,
    pub endpoint: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub token_limit: Option<u32>,
    #[serde(default)]
    pub ollama_think: Option<bool>,
}

impl Provider {
    pub fn local() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "Ollama Local".into(),
            kind: ProviderKind::Ollama,
            endpoint: ProviderKind::Ollama.endpoint().into(),
            models: vec![],
            has_secret: false,
            headers: vec![],
            token_limit: None,
            ollama_think: None,
        }
    }

    pub fn validate(&mut self) -> Result<()> {
        self.name = self.name.trim().into();
        ensure!(!self.name.is_empty(), "Enter a provider name.");
        if self.kind == ProviderKind::ChatGptAccount {
            self.endpoint.clear();
            self.headers.clear();
        } else {
            self.endpoint = self.endpoint.trim().trim_end_matches('/').into();
            endpoint(&self.endpoint)?;
            for (name, variable) in &self.headers {
                reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
                ensure!(
                    !matches!(
                        name.to_ascii_lowercase().as_str(),
                        "authorization"
                            | "x-api-key"
                            | "host"
                            | "content-type"
                            | "content-length"
                            | "transfer-encoding"
                    ),
                    "Header {name} is managed by the adapter."
                );
                validate_env(variable)?;
            }
        }
        normalize_models(&mut self.models)?;
        ensure!(self.token_limit != Some(0), "Token limit must be positive.");
        Ok(())
    }
}

pub fn normalize_models(models: &mut Vec<String>) -> Result<()> {
    let mut unique = std::collections::HashSet::new();
    for model in models.iter_mut() {
        *model = model.trim().into();
    }
    models.retain(|model| !model.is_empty() && unique.insert(model.clone()));
    ensure!(
        models
            .iter()
            .all(|model| model.len() <= 300 && !model.chars().any(char::is_control)),
        "A model ID is invalid."
    );
    Ok(())
}

pub fn endpoint(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).context("Enter a valid HTTP or HTTPS endpoint.")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Use HTTP or HTTPS."
    );
    ensure!(url.host().is_some(), "Endpoint must include a host.");
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none(),
        "Use the key or header controls for credentials. Endpoints cannot contain credentials, a query, or a fragment."
    );
    Ok(url)
}

pub fn validate_env(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
        "Invalid environment variable name: {name}"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Stdio,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub transport: Transport,
    pub address: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub blocked_tools: Vec<String>,
    #[serde(default)]
    pub current_protocol: bool,
}

impl Server {
    pub fn validate(&mut self) -> Result<()> {
        self.name = self.name.trim().into();
        self.address = self.address.trim().into();
        ensure!(
            !self.name.is_empty() && !self.address.is_empty(),
            "Enter a server name and address."
        );
        match self.transport {
            Transport::Http => {
                endpoint(&self.address)?;
            }
            Transport::Stdio => {
                for variable in &self.env {
                    validate_env(variable)?;
                }
                ensure!(
                    self.args.iter().all(|arg| !arg.contains('\0')),
                    "An argument contains a null byte."
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    Deny,
    Ask,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    ReadOnly,
    Workspace,
    FullAccess,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    pub profile: Profile,
    pub read: Rule,
    pub write: Rule,
    pub command: Rule,
    pub mcp: Rule,
    #[serde(default = "default_steps")]
    pub max_steps: u8,
    #[serde(default)]
    pub network: bool,
}

fn default_steps() -> u8 {
    16
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            profile: Profile::Workspace,
            read: Rule::Allow,
            write: Rule::Ask,
            command: Rule::Ask,
            mcp: Rule::Ask,
            max_steps: default_steps(),
            network: false,
        }
    }
}

impl Permissions {
    /// Resolve a named profile on the backend, regardless of submitted custom values.
    pub fn effective(&self) -> Self {
        let (read, write, command, mcp, network) = match self.profile {
            Profile::ReadOnly => (Rule::Allow, Rule::Deny, Rule::Deny, Rule::Deny, false),
            Profile::Workspace => (Rule::Allow, Rule::Ask, Rule::Ask, Rule::Ask, false),
            Profile::FullAccess => (Rule::Allow, Rule::Allow, Rule::Allow, Rule::Ask, true),
            Profile::Custom => (self.read, self.write, self.command, self.mcp, self.network),
        };
        Self {
            profile: self.profile,
            read,
            write,
            command,
            mcp,
            max_steps: self.max_steps.clamp(1, 50),
            network,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub accent: String,
    pub font_scale: u16,
    pub density: String,
    pub font: String,
    pub width: u16,
    pub sidebar: u16,
    pub radius: u8,
    pub motion: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            accent: "#c1f483".into(),
            font_scale: 100,
            density: "comfortable".into(),
            font: "system".into(),
            width: 850,
            sidebar: 252,
            radius: 15,
            motion: true,
        }
    }
}

impl Appearance {
    fn sanitize(&mut self) {
        let defaults = Self::default();
        if !matches!(self.theme.as_str(), "dark" | "light" | "system") {
            self.theme = defaults.theme;
        }
        if !matches!(
            self.density.as_str(),
            "compact" | "comfortable" | "spacious"
        ) {
            self.density = defaults.density;
        }
        if !matches!(self.font.as_str(), "system" | "mono" | "serif") {
            self.font = defaults.font;
        }
        if self.accent.len() != 7
            || !self.accent.starts_with('#')
            || !self.accent.as_bytes()[1..]
                .iter()
                .all(u8::is_ascii_hexdigit)
        {
            self.accent = defaults.accent;
        }
        self.font_scale = self.font_scale.clamp(80, 140);
        self.width = self.width.clamp(600, 1400);
        self.sidebar = self.sidebar.clamp(185, 355);
        self.radius = self.radius.min(28);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Chat,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    #[serde(default)]
    pub incomplete: bool,
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<ToolCall>,
    /// Responses reasoning items only exist for the duration of the tool loop.
    #[serde(skip)]
    pub output_items: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<Attachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub text: String,
    /// When present, text contains the original bytes encoded in base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl Attachment {
    pub fn data_url(&self) -> Option<String> {
        self.media_type
            .as_ref()
            .map(|mime| format!("data:{mime};base64,{}", self.text))
    }
}

impl Message {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            incomplete: false,
            role,
            text: text.into(),
            calls: vec![],
            output_items: vec![],
            tool_id: None,
            tool_name: None,
            tool_label: None,
            provider: None,
            model: None,
            attachment: None,
        }
    }

    pub fn content(&self) -> String {
        match &self.attachment {
            Some(attachment) if attachment.media_type.is_some() => {
                format!("{}\n\n[Attached: {}]", self.text, attachment.name)
            }
            Some(attachment) => format!(
                "{}\n\n<user_attachment name={:?}>\n{}\n</user_attachment>",
                self.text, attachment.name, attachment.text
            ),
            None => self.text.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    pub mode: Mode,
    pub title: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub providers: Vec<Provider>,
    pub servers: Vec<Server>,
    pub threads: Vec<Thread>,
    pub projects: Vec<Project>,
    pub permissions: Permissions,
    pub appearance: Appearance,
    pub codex_path: String,
    pub instructions: String,
    pub active_thread: Option<Uuid>,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            providers: vec![Provider::local()],
            servers: vec![],
            threads: vec![],
            projects: vec![],
            permissions: Permissions::default(),
            appearance: Appearance::default(),
            codex_path: "codex".into(),
            instructions: String::new(),
            active_thread: None,
        }
    }
}

impl Data {
    pub fn project_for_root(&mut self, root: PathBuf) -> Project {
        if let Some(project) = self.projects.iter().find(|project| project.root == root) {
            return project.clone();
        }
        let name = root
            .file_name()
            .unwrap_or(root.as_os_str())
            .to_string_lossy()
            .into();
        let project = Project {
            id: Uuid::new_v4(),
            name,
            root,
        };
        self.projects.push(project.clone());
        project
    }

    // Earlier local builds stored only a working directory on each thread.
    fn migrate_project_links(&mut self) -> bool {
        let ungrouped: Vec<_> = self
            .threads
            .iter()
            .enumerate()
            .filter_map(|(index, thread)| {
                if thread.mode == Mode::Agent && thread.project_id.is_none() {
                    thread.workspace.clone().map(|root| (index, root))
                } else {
                    None
                }
            })
            .collect();
        let changed = !ungrouped.is_empty();
        for (index, root) in ungrouped {
            let root = root.canonicalize().unwrap_or(root);
            let project = self.project_for_root(root);
            self.threads[index].project_id = Some(project.id);
        }
        changed
    }
}

/// A small JSON store written through a same-directory temp file. Mutations become
/// visible to readers only after the replacement has succeeded.
pub struct Store {
    _lock: fs::File,
    path: PathBuf,
    data: Mutex<Data>,
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("workspace.lock"))?;
        lock.try_lock()
            .context("This OpenDex data folder is already in use.")?;
        let path = directory.join("workspace.json");
        let mut data: Data = match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("Could not read OpenDex workspace.json.")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let data = Data::default();
                Self::write(&path, &data)?;
                data
            }
            Err(error) => return Err(error.into()),
        };
        if data.migrate_project_links() {
            Self::write(&path, &data)?;
        }
        Ok(Self {
            _lock: lock,
            path,
            data: Mutex::new(data),
        })
    }

    pub fn snapshot(&self) -> Data {
        self.data.lock().expect("store lock").clone()
    }

    pub fn change<T>(&self, update: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        let mut data = self.data.lock().expect("store lock");
        let mut next = data.clone();
        let result = update(&mut next)?;
        Self::write(&self.path, &next)?;
        *data = next;
        Ok(result)
    }

    fn write(path: &Path, data: &Data) -> Result<()> {
        let mut file =
            tempfile::NamedTempFile::new_in(path.parent().context("Missing app data folder.")?)?;
        serde_json::to_writer(&mut file, data)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }

    pub fn update_settings(
        &self,
        mut appearance: Appearance,
        mut permissions: Permissions,
        codex_path: String,
        instructions: String,
    ) -> Result<()> {
        appearance.sanitize();
        permissions.max_steps = permissions.max_steps.clamp(1, 50);
        self.change(|data| {
            data.appearance = appearance;
            data.permissions = permissions;
            data.codex_path = codex_path.trim().to_owned();
            data.instructions = instructions;
            Ok(())
        })
    }
}

/// Bind stored keys to the original endpoint so editing a URL never forwards an old key.
fn credential(id: Uuid, target: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        "org.opendex.desktop",
        &format!("{id}:{target}"),
    )?)
}

pub fn read_secret(id: Uuid, target: &str, expected: bool) -> Result<Option<String>> {
    if !expected {
        return Ok(None);
    }
    match credential(id, target)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => bail!("The saved key is missing. Re-enter it in Settings."),
        Err(error) => Err(error).context("Could not read the system credential store."),
    }
}

pub fn write_secret(id: Uuid, target: &str, secret: &str) -> Result<()> {
    let secret = secret.trim();
    ensure!(
        !secret.is_empty() && !secret.chars().any(char::is_control),
        "Enter a valid key."
    );
    credential(id, target)?
        .set_password(secret)
        .context("Could not save the key in the system credential store.")
}

pub fn delete_secret(id: Uuid, target: &str) -> Result<()> {
    match credential(id, target)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => {
            Err(error).context("Could not remove the key from the system credential store.")
        }
    }
}
