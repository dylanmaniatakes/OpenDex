//! Persistent app data. Credentials stay in the system credential store.
use crate::{images, web};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
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
    Gemini,
    Ollama,
    OllamaCloud,
    ChatGptAccount,
}

#[derive(Debug, Clone, Default, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicAuth {
    #[default]
    Api,
    Account,
}

impl ProviderKind {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::OpenAiCompatible => "http://127.0.0.1:1234/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_header: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub token_limit: Option<u32>,
    #[serde(default)]
    pub ollama_think: Option<bool>,
    #[serde(default)]
    pub anthropic_auth: AnthropicAuth,
    #[serde(default = "default_claude_path")]
    pub claude_path: String,
}

fn default_claude_path() -> String {
    "claude".into()
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
            secret_ref: None,
            api_key_header: None,
            headers: vec![],
            token_limit: None,
            ollama_think: None,
            anthropic_auth: AnthropicAuth::Api,
            claude_path: default_claude_path(),
        }
    }

    pub fn credential_id(&self) -> Uuid {
        self.secret_ref.unwrap_or(self.id)
    }

    pub fn key_header(&self) -> Option<&str> {
        self.api_key_header.as_deref().or(match self.kind {
            ProviderKind::Anthropic if self.anthropic_auth == AnthropicAuth::Api => {
                Some("x-api-key")
            }
            ProviderKind::Gemini => Some("x-goog-api-key"),
            _ => None,
        })
    }

    pub fn validate(&mut self) -> Result<()> {
        self.claude_path = self.claude_path.trim().to_owned();
        if self.claude_path.is_empty() {
            self.claude_path = default_claude_path();
        }
        self.name = self.name.trim().into();
        ensure!(!self.name.is_empty(), "Enter a provider name.");
        if self.kind == ProviderKind::ChatGptAccount {
            self.endpoint.clear();
            self.api_key_header = None;
            self.headers.clear();
        } else {
            self.endpoint = self.endpoint.trim().trim_end_matches('/').into();
            endpoint(&self.endpoint)?;
            self.api_key_header = self
                .api_key_header
                .take()
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty());
            if let Some(name) = &self.api_key_header {
                validate_request_header(name)?;
            }
            let mut seen = std::collections::HashSet::new();
            for (name, variable) in &self.headers {
                let name = validate_request_header(name)?;
                ensure!(
                    !matches!(name.as_str(), "authorization" | "x-api-key"),
                    "Use the API key and key header fields for {name}."
                );
                ensure!(
                    seen.insert(name),
                    "A request header is configured more than once."
                );
                validate_env(variable)?;
            }
        }
        normalize_models(&mut self.models)?;
        ensure!(self.token_limit != Some(0), "Token limit must be positive.");
        if self.anthropic_auth == AnthropicAuth::Account {
            if self.kind == ProviderKind::Anthropic {
                ensure!(
                    self.endpoint == ProviderKind::Anthropic.endpoint(),
                    "Account login uses the official Anthropic endpoint."
                );
                self.api_key_header = None;
            } else {
                self.anthropic_auth = AnthropicAuth::Api;
            }
        }
        Ok(())
    }
}

fn validate_request_header(name: &str) -> Result<reqwest::header::HeaderName> {
    let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
    ensure!(
        !matches!(
            header.as_str(),
            "host"
                | "content-type"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "upgrade"
                | "te"
                | "trailer"
                | "proxy-authorization"
                | "expect"
        ),
        "Header {name} is managed by the HTTP client."
    );
    Ok(header)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<Uuid>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub blocked_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_permission: Option<Rule>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_permissions: BTreeMap<String, Rule>,
    #[serde(default)]
    pub current_protocol: bool,
    /// HTTP header name paired with the environment variable that supplies its value.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

impl Server {
    pub fn credential_id(&self) -> Uuid {
        self.secret_ref.unwrap_or(self.id)
    }

    /// Server and tool rules can only narrow the active permission profile.
    /// Existing blocks remain in force for configurations saved by older builds.
    pub fn tool_rule(&self, global: Rule, name: &str) -> Rule {
        if self.blocked_tools.iter().any(|blocked| blocked == name) {
            return Rule::Deny;
        }
        self.server_tool_rule(global).min(
            self.tool_permissions
                .get(name)
                .copied()
                .unwrap_or(Rule::Allow),
        )
    }

    pub fn server_tool_rule(&self, global: Rule) -> Rule {
        global.min(self.tool_permission.unwrap_or(Rule::Allow))
    }

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
                self.env.clear();
                let mut seen = std::collections::HashSet::new();
                for (name, variable) in &self.headers {
                    let name = validate_request_header(name)?;
                    ensure!(
                        !matches!(
                            name.as_str(),
                            "authorization" | "mcp-session-id" | "mcp-protocol-version"
                        ),
                        "Header {name} is managed by the MCP client or bearer key."
                    );
                    ensure!(
                        seen.insert(name),
                        "A request header is configured more than once."
                    );
                    validate_env(variable)?;
                }
            }
            Transport::Stdio => {
                for variable in &self.env {
                    validate_env(variable)?;
                }
                ensure!(
                    self.args.iter().all(|arg| !arg.contains('\0')),
                    "An argument contains a null byte."
                );
                self.headers.clear();
            }
        }
        Ok(())
    }
}

// Declaration order is from most to least restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    #[serde(default = "ask")]
    pub web: Rule,
    #[serde(default = "default_steps")]
    pub max_steps: u8,
    #[serde(default)]
    pub network: bool,
}

fn default_steps() -> u8 {
    16
}

fn ask() -> Rule {
    Rule::Ask
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            profile: Profile::Workspace,
            read: Rule::Allow,
            write: Rule::Ask,
            command: Rule::Ask,
            mcp: Rule::Ask,
            web: Rule::Ask,
            max_steps: default_steps(),
            network: false,
        }
    }
}

impl Permissions {
    /// Resolve a named profile on the backend, regardless of submitted custom values.
    pub fn effective(&self) -> Self {
        let (read, write, command, mcp, web, network) = match self.profile {
            Profile::ReadOnly => (
                Rule::Allow,
                Rule::Deny,
                Rule::Deny,
                Rule::Deny,
                Rule::Deny,
                false,
            ),
            Profile::Workspace => (
                Rule::Allow,
                Rule::Ask,
                Rule::Ask,
                Rule::Ask,
                Rule::Ask,
                false,
            ),
            Profile::FullAccess => (
                Rule::Allow,
                Rule::Allow,
                Rule::Allow,
                Rule::Ask,
                Rule::Ask,
                true,
            ),
            Profile::Custom => (
                self.read,
                self.write,
                self.command,
                self.mcp,
                self.web,
                self.network,
            ),
        };
        Self {
            profile: self.profile,
            read,
            write,
            command,
            mcp,
            web,
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
    pub(crate) fn sanitize(&mut self) {
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

pub const MAX_RESOURCE_BYTES: usize = 128_000;
pub const MAX_RESOURCES_PER_MESSAGE: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    pub server_id: Uuid,
    pub source: String,
    pub name: String,
    pub uri: String,
    pub text: String,
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
    /// Native provider output exists only for the duration of the tool loop.
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
    pub image_generation: Option<images::Config>,
    #[serde(
        default,
        alias = "attachment",
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<McpResource>,
}

/// Older workspace files and send requests can contain a single attachment.
pub(crate) fn one_or_many<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored<T> {
        Many(Vec<T>),
        One(Option<T>),
    }
    Ok(match Stored::deserialize(deserializer)? {
        Stored::Many(items) => items,
        Stored::One(item) => item.into_iter().collect(),
    })
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
            image_generation: None,
            attachments: vec![],
            resources: vec![],
        }
    }

    pub fn has_content(&self) -> bool {
        !self.text.is_empty() || !self.attachments.is_empty() || !self.resources.is_empty()
    }

    pub fn validate_user_content(&self) -> Result<()> {
        ensure!(
            !self.text.trim().is_empty()
                || !self.attachments.is_empty()
                || !self.resources.is_empty(),
            "Write a message or attach a file or resource."
        );
        ensure!(
            self.text.len() <= 256_000,
            "This message exceeds the size limit."
        );
        Ok(())
    }

    pub fn media_attachments(&self) -> impl Iterator<Item = (&Attachment, &str)> {
        self.attachments
            .iter()
            .filter_map(|file| file.media_type.as_deref().map(|mime| (file, mime)))
    }

    pub fn content(&self) -> String {
        use std::fmt::Write as _;
        let mut content = self.text.clone();
        for attachment in &self.attachments {
            if attachment.media_type.is_some() {
                let _ = write!(content, "\n\n[Attached: {}]", attachment.name);
            } else {
                let _ = write!(
                    content,
                    "\n\n<user_attachment name={:?}>\n{}\n</user_attachment>",
                    attachment.name, attachment.text
                );
            }
        }
        for resource in &self.resources {
            let _ = write!(
                content,
                "\n\n<user_mcp_resource server={:?} uri={:?}>\n{}\n</user_mcp_resource>",
                resource.source, resource.uri, resource.text
            );
        }
        content
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchAction {
    Continue,
    Retry,
    Edit { text: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "servers", rename_all = "snake_case")]
pub enum McpScope {
    #[default]
    All,
    Selected(Vec<Uuid>),
}

impl McpScope {
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// An empty selection deliberately gives the conversation no MCP servers.
    pub fn includes(&self, id: Uuid) -> bool {
        match self {
            Self::All => true,
            Self::Selected(servers) => servers.contains(&id),
        }
    }
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
    #[serde(default, skip_serializing_if = "McpScope::is_all")]
    pub mcp_scope: McpScope,
    #[serde(default)]
    pub web_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread: Option<Uuid>,
}

impl Thread {
    pub(crate) fn append_user_message(&mut self, message: Message) {
        if self.messages.is_empty() && self.title == "New conversation" {
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
            self.title = title.chars().take(58).collect();
            if title.chars().count() > 58 {
                self.title.push('…');
            }
        }
        self.messages.push(message);
    }

    pub fn branch_at(&self, message: Uuid, action: BranchAction) -> Result<Self> {
        let position = self
            .messages
            .iter()
            .position(|item| item.id == message)
            .context("That message is no longer in the conversation.")?;
        let selected = &self.messages[position];
        ensure!(
            selected.role != Role::Tool && selected.has_content(),
            "Choose a chat message to branch from."
        );
        let mut end = position;
        let label = match &action {
            BranchAction::Continue => "Branch",
            BranchAction::Retry => {
                ensure!(self.mode == Mode::Chat, "Retry is available in Model Chat.");
                end = self.messages[..=position]
                    .iter()
                    .rposition(|item| item.role == Role::User)
                    .context("This response has no user message to retry.")?;
                ensure!(
                    self.messages[end].image_generation.is_none(),
                    "Use Generate image again for this prompt."
                );
                "Retry"
            }
            BranchAction::Edit { .. } => {
                ensure!(
                    self.mode == Mode::Chat
                        && selected.role == Role::User
                        && selected.image_generation.is_none(),
                    "Choose a user message in Model Chat to edit."
                );
                "Edited"
            }
        };
        let mut messages = self.messages[..=end].to_vec();
        if let BranchAction::Edit { text } = action {
            let prompt = messages.last_mut().expect("selected message exists");
            prompt.id = Uuid::new_v4();
            prompt.text = text.trim().into();
            prompt.validate_user_content()?;
        }
        let prefix: String = self.title.chars().take(130 - label.len() - 3).collect();
        Ok(Self {
            id: Uuid::new_v4(),
            mode: self.mode,
            title: format!("{prefix} · {label}"),
            messages,
            workspace: self.workspace.clone(),
            project_id: self.project_id,
            provider_id: self.provider_id,
            model_id: self.model_id.clone(),
            instructions: self.instructions.clone(),
            mcp_scope: self.mcp_scope.clone(),
            web_enabled: self.web_enabled,
            parent_thread: Some(self.id),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub providers: Vec<Provider>,
    pub servers: Vec<Server>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search: Option<web::Config>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_generation: Option<images::Config>,
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
            web_search: None,
            image_generation: None,
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
    pub fn add_provider_models(&mut self, id: Uuid, mut models: Vec<String>) -> Result<Provider> {
        normalize_models(&mut models)?;
        let connection = self
            .providers
            .iter_mut()
            .find(|item| item.id == id)
            .context("That provider no longer exists.")?;
        connection.models.extend(models);
        normalize_models(&mut connection.models)?;
        Ok(connection.clone())
    }

    pub fn remove_provider_model(&mut self, id: Uuid, model: &str) -> Result<Provider> {
        let connection = self
            .providers
            .iter_mut()
            .find(|item| item.id == id)
            .context("That provider no longer exists.")?;
        connection.models.retain(|item| item != model);
        Ok(connection.clone())
    }

    pub(crate) fn set_web_search(&mut self, search: Option<web::Config>) {
        if self.web_search != search {
            self.web_search = search;
            for thread in &mut self.threads {
                thread.web_enabled = false;
            }
        }
    }

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

pub(crate) fn open_lock_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// A small JSON store written through a same-directory temp file. Mutations become
/// visible to readers only after the replacement has succeeded.
pub struct Store {
    lock: fs::File,
    path: PathBuf,
    data: Mutex<Data>,
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let lock = open_lock_file(&directory.join("workspace.lock"))?;
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
            lock,
            path,
            data: Mutex::new(data),
        })
    }

    pub fn codex_home(&self) -> PathBuf {
        self.path
            .parent()
            .expect("workspace directory")
            .join("codex")
    }

    pub fn snapshot(&self) -> Data {
        self.data.lock().expect("store lock").clone()
    }

    pub(crate) fn inspect<T>(&self, inspect: impl FnOnce(&Data) -> T) -> T {
        inspect(&self.data.lock().expect("store lock"))
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

impl Drop for Store {
    fn drop(&mut self) {
        // A concurrent subprocess can briefly inherit the descriptor until exec.
        let _ = self.lock.unlock();
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn closing_a_workspace_releases_the_lock_with_an_inherited_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path()).unwrap();
        let inherited = store.lock.try_clone().unwrap();
        assert!(Store::open(directory.path()).is_err());
        drop(store);
        let reopened = Store::open(directory.path()).unwrap();
        drop(inherited);
        assert!(Store::open(directory.path()).is_err());
        drop(reopened);
    }

    #[test]
    fn anthropic_account_mode_keeps_the_official_endpoint() {
        let mut provider = Provider::local();
        provider.kind = ProviderKind::Anthropic;
        provider.anthropic_auth = AnthropicAuth::Account;
        provider.endpoint = "https://gateway.example.org/v1".into();
        assert!(provider.validate().is_err());

        provider.endpoint = ProviderKind::Anthropic.endpoint().into();
        provider.validate().unwrap();
        assert_eq!(provider.anthropic_auth, AnthropicAuth::Account);

        provider.anthropic_auth = AnthropicAuth::Api;
        provider.endpoint = "https://gateway.example.org/v1".into();
        provider.validate().unwrap();
    }
}
