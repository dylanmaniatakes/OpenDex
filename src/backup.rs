//! Encrypted, versioned settings archives. Account sessions remain owned by their provider.

use crate::{
    agent, codex, images,
    state::{
        Appearance, Data, McpScope, Permissions, Profile, Project, Provider, ProviderKind, Server,
        Store, Transport, delete_secret, read_secret, write_secret,
    },
    transcription, voice, web,
};
use age::secrecy::SecretString;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const FORMAT: &str = "opendex-settings";
const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
// scrypt N=2^17, r=8 and p=1 requires 128 MiB. A fixed factor makes
// the protection consistent across builds and lets smaller machines restore backups.
const SCRYPT_WORK_FACTOR: u8 = 17;

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct ArchiveKey(String);

impl Drop for ArchiveKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct Connection<T> {
    settings: T,
    key: Option<ArchiveKey>,
}

#[derive(Serialize, Deserialize)]
struct VoiceArchive {
    profiles: Vec<Connection<voice::Profile>>,
    selected: Option<Uuid>,
    autoplay: bool,
}

#[derive(Serialize, Deserialize)]
struct DictationArchive {
    settings: transcription::Settings,
    key: Option<ArchiveKey>,
}

impl VoiceArchive {
    fn capture(settings: &voice::Settings) -> Result<Self> {
        let profiles = settings
            .profiles
            .iter()
            .map(|profile| {
                let key = profile
                    .secret()
                    .with_context(|| format!("Could not back up the key for {}.", profile.name))?;
                let mut settings = profile.clone();
                settings.secret_ref = None;
                Ok(Connection {
                    settings,
                    key: key.map(ArchiveKey),
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            profiles,
            selected: settings.selected,
            autoplay: settings.autoplay,
        })
    }

    fn validate(&mut self) -> Result<()> {
        let mut ids = HashSet::new();
        for entry in &mut self.profiles {
            entry.settings.validate()?;
            ensure!(
                ids.insert(entry.settings.id),
                "The backup contains duplicate voice connection IDs."
            );
            entry.settings.secret_ref = None;
            entry.settings.has_secret = entry.key.is_some();
        }
        ensure!(
            self.selected.is_none_or(|id| ids.contains(&id)),
            "The active voice is missing from the backup."
        );
        self.autoplay &= self.selected.is_some();
        Ok(())
    }

    fn into_settings(self) -> voice::Settings {
        voice::Settings {
            profiles: self
                .profiles
                .into_iter()
                .map(|entry| entry.settings)
                .collect(),
            selected: self.selected,
            autoplay: self.autoplay,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Archive {
    format: String,
    version: u32,
    providers: Vec<Connection<Provider>>,
    servers: Vec<Connection<Server>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    web_search: Option<web::Config>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_generation: Option<images::Config>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_sources: Option<Vec<Connection<images::Source>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_source_default: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    voice: Option<VoiceArchive>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dictation: Option<DictationArchive>,
    projects: Vec<Project>,
    permissions: Permissions,
    appearance: Appearance,
    instructions: String,
    codex_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chatgpt_session: Option<ArchiveKey>,
}

#[derive(Serialize)]
pub struct ProjectPreview {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    pub available: bool,
}

#[derive(Serialize)]
pub struct Preview {
    pub checksum: [u8; 32],
    pub providers: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub web_search: Option<&'static str>,
    pub image_model: Option<String>,
    pub image_sources: Option<Vec<String>>,
    pub voice_profiles: Option<Vec<String>>,
    pub local_servers: Vec<String>,
    pub projects: Vec<ProjectPreview>,
    pub has_chatgpt_connection: bool,
    pub has_chatgpt_session: bool,
    pub can_restore_chatgpt_session: bool,
    pub permission_profile: Profile,
    pub codex_path: String,
}

#[derive(Serialize)]
pub struct RestoreResult {
    pub obsolete_keys_remaining: usize,
    pub chatgpt_session_skipped: bool,
}

impl Archive {
    fn capture(data: &Data, home: &Path) -> Result<Self> {
        let providers = data
            .providers
            .iter()
            .map(|provider| {
                let key = read_secret(
                    provider.credential_id(),
                    &provider.endpoint,
                    provider.has_secret,
                )
                .with_context(|| format!("Could not back up the key for {}.", provider.name))?;
                let mut settings = provider.clone();
                settings.secret_ref = None;
                Ok(Connection {
                    settings,
                    key: key.map(ArchiveKey),
                })
            })
            .collect::<Result<_>>()?;
        let servers = data
            .servers
            .iter()
            .map(|server| {
                let key = read_secret(server.credential_id(), &server.address, server.has_secret)
                    .with_context(|| {
                    format!("Could not back up the key for {}.", server.name)
                })?;
                let mut settings = server.clone();
                settings.secret_ref = None;
                Ok(Connection {
                    settings,
                    key: key.map(ArchiveKey),
                })
            })
            .collect::<Result<_>>()?;
        let image_sources = data
            .image_sources
            .iter()
            .map(|source| {
                let key = source
                    .secret()
                    .with_context(|| format!("Could not back up the key for {}.", source.name))?;
                let mut settings = source.clone();
                settings.secret_ref = None;
                Ok(Connection {
                    settings,
                    key: key.map(ArchiveKey),
                })
            })
            .collect::<Result<_>>()?;
        let mut dictation = data.dictation.clone();
        let key = if let Some(endpoint) = &mut dictation.endpoint {
            let key = endpoint
                .secret()
                .context("Could not back up the dictation endpoint key.")?;
            endpoint.secret_ref = None;
            key.map(ArchiveKey)
        } else {
            None
        };
        Ok(Self {
            chatgpt_session: if data
                .providers
                .iter()
                .any(|item| item.kind == ProviderKind::ChatGptAccount)
            {
                read_codex_session(home)?.map(|secret| ArchiveKey(secret.to_string()))
            } else {
                None
            },
            format: FORMAT.into(),
            version: SCHEMA_VERSION,
            providers,
            servers,
            web_search: data.web_search.clone(),
            image_generation: data.image_generation.clone(),
            image_sources: Some(image_sources),
            image_source_default: data.image_source_default,
            voice: Some(VoiceArchive::capture(&data.voice)?),
            dictation: Some(DictationArchive {
                settings: dictation,
                key,
            }),
            projects: data.projects.clone(),
            permissions: data.permissions.clone(),
            appearance: data.appearance.clone(),
            instructions: data.instructions.clone(),
            codex_path: data.codex_path.clone(),
        })
    }

    fn validate(&mut self) -> Result<()> {
        ensure!(
            self.format == FORMAT,
            "This file is not an OpenDex settings backup."
        );
        ensure!(
            self.version == SCHEMA_VERSION,
            "This backup uses an unsupported OpenDex version."
        );
        if let Some(session) = &self.chatgpt_session {
            validate_codex_session(&session.0)?;
            ensure!(
                self.providers
                    .iter()
                    .any(|item| item.settings.kind == ProviderKind::ChatGptAccount),
                "The backup contains a session without a ChatGPT connection."
            );
        }
        let mut ids = HashSet::new();
        let mut account = false;
        for provider in &mut self.providers {
            provider.settings.validate()?;
            ensure!(
                ids.insert(provider.settings.id),
                "The backup contains duplicate connection IDs."
            );
            if provider.settings.kind == ProviderKind::ChatGptAccount {
                ensure!(
                    !account && provider.key.is_none(),
                    "The backup contains an invalid ChatGPT connection."
                );
                account = true;
            }
            provider.settings.has_secret = provider.key.is_some();
            provider.settings.secret_ref = None;
        }
        if let Some(search) = &self.web_search {
            search.resolve(self.providers.iter().map(|entry| &entry.settings))?;
        }
        let mut image_ids = HashSet::new();
        let mut image_names = HashSet::new();
        if let Some(sources) = &mut self.image_sources {
            ensure!(
                sources.len() <= 32,
                "The backup has too many image sources."
            );
            for source in sources {
                source.settings.validate()?;
                ensure!(
                    ids.insert(source.settings.id)
                        && image_ids.insert(source.settings.id)
                        && image_names.insert(source.settings.name.to_lowercase()),
                    "The backup contains duplicate image sources."
                );
                source.settings.secret_ref = None;
                source.settings.has_secret = source.key.is_some();
            }
        }
        if let Some(config) = &mut self.image_generation {
            config.validate()?;
            if !image_ids.contains(&config.provider) {
                config.resolve(self.providers.iter().map(|entry| &entry.settings))?;
            }
        }
        if let Some(voice) = &mut self.voice {
            voice.validate()?;
        }
        if let Some(dictation) = &mut self.dictation {
            dictation.settings.validate()?;
            if let Some(endpoint) = &mut dictation.settings.endpoint {
                endpoint.secret_ref = None;
                endpoint.has_secret = dictation.key.is_some();
            } else {
                ensure!(
                    dictation.key.is_none(),
                    "The backup includes a dictation key without an endpoint."
                );
            }
        }
        ensure!(
            self.image_source_default
                .is_none_or(|id| image_ids.contains(&id)),
            "The default image source is missing from the backup."
        );
        for server in &mut self.servers {
            server.settings.validate()?;
            ensure!(
                ids.insert(server.settings.id),
                "The backup contains duplicate connection IDs."
            );
            ensure!(
                server.settings.transport == Transport::Http || server.key.is_none(),
                "The backup contains a bearer key for a local MCP server."
            );
            server.settings.has_secret = server.key.is_some();
            server.settings.secret_ref = None;
        }
        let mut project_ids = HashSet::new();
        ensure!(
            self.projects
                .iter()
                .all(|project| project_ids.insert(project.id)),
            "The backup contains duplicate project IDs."
        );
        self.permissions.max_steps = self.permissions.max_steps.clamp(1, 50);
        self.appearance.sanitize();
        Ok(())
    }

    pub fn preview(&self, checksum: [u8; 32]) -> Preview {
        Preview {
            checksum,
            web_search: self.web_search.as_ref().map(web::Config::name),
            image_model: self
                .image_generation
                .as_ref()
                .map(|config| config.model.clone()),
            image_sources: self.image_sources.as_ref().map(|sources| {
                sources
                    .iter()
                    .map(|source| source.settings.name.clone())
                    .collect()
            }),
            voice_profiles: self.voice.as_ref().map(|voice| {
                voice
                    .profiles
                    .iter()
                    .map(|entry| entry.settings.name.clone())
                    .collect()
            }),
            providers: self
                .providers
                .iter()
                .map(|item| item.settings.name.clone())
                .collect(),
            mcp_servers: self
                .servers
                .iter()
                .map(|item| item.settings.name.clone())
                .collect(),
            local_servers: self
                .servers
                .iter()
                .filter_map(|item| {
                    let server = &item.settings;
                    (server.enabled && server.transport == Transport::Stdio)
                        .then(|| format!("{} ({})", server.name, server.address))
                })
                .collect(),
            projects: self
                .projects
                .iter()
                .map(|project| ProjectPreview {
                    id: project.id,
                    name: project.name.clone(),
                    path: project.root.clone(),
                    available: project.root.is_absolute() && project.root.is_dir(),
                })
                .collect(),
            has_chatgpt_session: self.chatgpt_session.is_some(),
            can_restore_chatgpt_session: !cfg!(target_os = "windows"),
            has_chatgpt_connection: self
                .providers
                .iter()
                .any(|item| item.settings.kind == ProviderKind::ChatGptAccount),
            codex_path: self.codex_path.clone(),
            permission_profile: self.permissions.profile,
        }
    }

    /// Write restored keys to new slots before replacing any settings. Failure leaves the
    /// old workspace file and its credentials in place, even when settings reuse their IDs.
    pub fn restore(self, store: &Store, remap: HashMap<Uuid, PathBuf>) -> Result<RestoreResult> {
        self.restore_with_session_support(store, remap, !cfg!(target_os = "windows"))
    }

    fn restore_with_session_support(
        mut self,
        store: &Store,
        remap: HashMap<Uuid, PathBuf>,
        session_supported: bool,
    ) -> Result<RestoreResult> {
        for (id, path) in remap {
            let project = self
                .projects
                .iter_mut()
                .find(|project| project.id == id)
                .context("A project to remap is missing from the backup.")?;
            project.root = agent::checked_root(&path)?;
        }
        let session = self.chatgpt_session.take();
        let chatgpt_session_skipped = session.is_some() && !session_supported;
        let mut session_swap = session
            .as_ref()
            .filter(|_| session_supported)
            .map(|session| SessionSwap::stage(&store.codex_home(), &session.0))
            .transpose()?;
        let mut staged = StagedKeys::default();
        let previous = store.change(|data| {
            let mut previous: Vec<_> = data
                .providers
                .iter()
                .filter(|provider| provider.has_secret)
                .map(|provider| (provider.credential_id(), provider.endpoint.clone()))
                .chain(
                    data.servers
                        .iter()
                        .filter(|server| server.has_secret)
                        .map(|server| (server.credential_id(), server.address.clone())),
                )
                .collect();
            for entry in &mut self.providers {
                entry.settings.secret_ref =
                    staged.add(&entry.settings.endpoint, entry.key.as_ref())?;
            }
            for entry in &mut self.servers {
                entry.settings.secret_ref =
                    staged.add(&entry.settings.address, entry.key.as_ref())?;
            }
            if let Some(sources) = &mut self.image_sources {
                previous.extend(
                    data.image_sources
                        .iter()
                        .filter(|source| source.has_secret)
                        .map(|source| (source.credential_id(), source.endpoint.clone())),
                );
                for source in sources {
                    source.settings.secret_ref =
                        staged.add(&source.settings.endpoint, source.key.as_ref())?;
                }
            }
            if let Some(voice) = &mut self.voice {
                previous.extend(
                    data.voice
                        .profiles
                        .iter()
                        .filter(|profile| profile.has_secret)
                        .map(|profile| (profile.credential_id(), profile.endpoint.clone())),
                );
                for entry in &mut voice.profiles {
                    entry.settings.secret_ref =
                        staged.add(&entry.settings.endpoint, entry.key.as_ref())?;
                }
            }
            if let Some(dictation) = &mut self.dictation {
                if let Some(old) = data
                    .dictation
                    .endpoint
                    .as_ref()
                    .filter(|old| old.has_secret)
                {
                    previous.push((old.credential_id(), old.url.clone()));
                }
                if let Some(endpoint) = &mut dictation.settings.endpoint {
                    endpoint.secret_ref = staged.add(&endpoint.url, dictation.key.as_ref())?;
                }
            }
            self.apply(data);
            Ok(previous)
        })?;
        if let Some(swap) = &mut session_swap {
            swap.committed = true;
        }
        staged.0.clear();
        let obsolete_keys_remaining = previous
            .into_iter()
            .filter(|(id, target)| delete_secret(*id, target).is_err())
            .count();
        Ok(RestoreResult {
            obsolete_keys_remaining,
            chatgpt_session_skipped,
        })
    }

    fn apply(self, data: &mut Data) {
        data.providers = self
            .providers
            .into_iter()
            .map(|entry| entry.settings)
            .collect();
        data.servers = self
            .servers
            .into_iter()
            .map(|entry| entry.settings)
            .collect();
        data.set_web_search(self.web_search);
        if let Some(sources) = self.image_sources {
            data.image_sources = sources.into_iter().map(|source| source.settings).collect();
            data.image_generation = self.image_generation;
            data.image_source_default = self.image_source_default;
        } else if !data.image_generation.as_ref().is_some_and(|config| {
            data.image_sources
                .iter()
                .any(|source| source.id == config.provider)
        }) {
            data.image_generation = self.image_generation;
        }
        if let Some(voice) = self.voice {
            data.voice = voice.into_settings();
        }
        if let Some(dictation) = self.dictation {
            data.dictation = dictation.settings;
        }
        data.appearance = self.appearance;
        data.permissions = self.permissions;
        data.instructions = self.instructions;
        data.codex_path = self.codex_path;
        for mut project in self.projects {
            if project.root.is_absolute() {
                project.root = project.root.canonicalize().unwrap_or(project.root);
            }
            if data
                .projects
                .iter()
                .any(|existing| existing.root == project.root)
            {
                continue;
            }
            if data
                .projects
                .iter()
                .any(|existing| existing.id == project.id)
            {
                project.id = Uuid::new_v4();
            }
            data.projects.push(project);
        }
        for thread in &mut data.threads {
            if let Some(provider) = data
                .providers
                .iter()
                .find(|provider| Some(provider.id) == thread.provider_id)
            {
                if thread
                    .model_id
                    .as_ref()
                    .is_some_and(|model| !provider.models.contains(model))
                {
                    thread.model_id = None;
                }
            } else {
                thread.provider_id = None;
                thread.model_id = None;
            }
            if let McpScope::Selected(ids) = &mut thread.mcp_scope {
                ids.retain(|id| data.servers.iter().any(|server| server.id == *id));
            }
        }
    }
}

#[derive(Default)]
struct StagedKeys(Vec<(Uuid, String)>);

impl StagedKeys {
    fn add(&mut self, target: &str, key: Option<&ArchiveKey>) -> Result<Option<Uuid>> {
        let Some(key) = key else {
            return Ok(None);
        };
        let id = Uuid::new_v4();
        self.0.push((id, target.into()));
        write_secret(id, target, &key.0)?;
        Ok(Some(id))
    }
}

impl Drop for StagedKeys {
    fn drop(&mut self) {
        for (id, target) in &self.0 {
            let _ = delete_secret(*id, target);
        }
    }
}

fn validate_codex_session(raw: &str) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(raw).context("The saved ChatGPT session is not valid JSON.")?;
    ensure!(
        ["access_token", "refresh_token"]
            .iter()
            .all(|name| value["tokens"][name]
                .as_str()
                .is_some_and(|token| !token.is_empty())),
        "The saved ChatGPT session is missing its access or refresh token."
    );
    Ok(())
}

fn read_codex_session(home: &Path) -> Result<Option<Zeroizing<String>>> {
    // Windows defaults to Codex's encrypted-file backend. Its portable export
    // requires that backend's format and master-key handling.
    if cfg!(target_os = "windows") {
        return Ok(None);
    }
    match codex::credential_entry(home)?.get_password() {
        Ok(raw) => {
            let raw = Zeroizing::new(raw);
            validate_codex_session(&raw)?;
            Ok(Some(raw))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error)
            .context("Could not read OpenDex's ChatGPT session from the system credential store."),
    }
}

// Restore the previous session if saving workspace settings fails after staging.
struct SessionSwap {
    entry: keyring::Entry,
    previous: Option<Zeroizing<String>>,
    committed: bool,
}
impl SessionSwap {
    fn stage(home: &Path, session: &str) -> Result<Self> {
        ensure!(
            !cfg!(target_os = "windows"),
            "ChatGPT session restore currently supports Codex's direct keyring backend on macOS and Linux. Sign in on this computer instead."
        );
        validate_codex_session(session)?;
        let entry = codex::credential_entry(home)?;
        let previous = match entry.get_password() {
            Ok(raw) => Some(Zeroizing::new(raw)),
            Err(keyring::Error::NoEntry) => None,
            Err(error) => return Err(error.into()),
        };
        let swap = Self {
            entry,
            previous,
            committed: false,
        };
        swap.entry
            .set_password(session)
            .context("Could not restore the ChatGPT session.")?;
        Ok(swap)
    }
}
impl Drop for SessionSwap {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(previous) = &self.previous {
                let _ = self.entry.set_password(previous);
            } else {
                let _ = self.entry.delete_credential();
            }
        }
    }
}

pub fn export(store: &Store, path: &Path, password: String) -> Result<()> {
    let password = Zeroizing::new(password);
    ensure!(
        password.chars().count() >= 12 && !password.trim().is_empty(),
        "Use a backup passphrase of at least 12 characters."
    );
    let archive = store.inspect(|data| Archive::capture(data, &store.codex_home()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(password.as_str()));
    recipient.set_work_factor(SCRYPT_WORK_FACTOR);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?;
    let mut writer = encryptor.wrap_output(&mut file)?;
    serde_json::to_writer(&mut writer, &archive)?;
    writer.finish()?;
    ensure!(
        file.as_file().metadata()?.len() <= MAX_BYTES,
        "The settings backup is too large."
    );
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// Authenticate the complete ciphertext before decoding settings. The digest ties a
/// confirmed restore to the same bytes the GUI displayed in the preview.
pub fn open(
    path: &Path,
    password: String,
    expected: Option<[u8; 32]>,
) -> Result<(Archive, [u8; 32])> {
    let password = Zeroizing::new(password);
    ensure!(!password.is_empty(), "Enter the backup passphrase.");
    let metadata = fs::metadata(path).context("Could not read this backup file.")?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BYTES,
        "Choose a settings backup under 16 MB."
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Choose a settings backup under 16 MB."
    );
    let checksum: [u8; 32] = Sha256::digest(&bytes).into();
    ensure!(
        expected.is_none_or(|value| value == checksum),
        "The backup file changed. Preview it again."
    );
    let decryptor = age::Decryptor::new_buffered(bytes.as_slice())
        .context("This is not a supported encrypted backup.")?;
    ensure!(
        decryptor.is_scrypt(),
        "This backup was not encrypted with a passphrase."
    );
    let mut identity = age::scrypt::Identity::new(SecretString::from(password.as_str()));
    // Cap the memory cost of decoding a file supplied by the user.
    identity.set_max_work_factor(SCRYPT_WORK_FACTOR + 1);
    let mut plaintext = Zeroizing::new(Vec::new());
    decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .context("The passphrase is incorrect or the backup is damaged.")?
        .read_to_end(&mut plaintext)
        .context("The backup is incomplete or damaged.")?;
    let mut archive: Archive = serde_json::from_slice(&plaintext).map_err(|_| {
        anyhow::anyhow!("This file does not contain a valid OpenDex settings backup.")
    })?;
    archive.validate()?;
    Ok((archive, checksum))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_session_backend_keeps_settings_restore_available() {
        let folder = tempfile::tempdir().unwrap();
        let store = Store::open(folder.path()).unwrap();
        // Capture without a ChatGPT connection first, so the test never reads
        // the OS keyring. Inject only disposable session bytes afterward.
        let mut archive = Archive::capture(&Data::default(), &store.codex_home()).unwrap();
        archive.providers.push(Connection {
            settings: Provider {
                kind: ProviderKind::ChatGptAccount,
                endpoint: String::new(),
                name: "ChatGPT account".into(),
                ..Provider::local()
            },
            key: None,
        });
        archive.instructions = "Restored instructions".into();
        archive.chatgpt_session = Some(ArchiveKey(serde_json::json!({"tokens":{"access_token":"dummy-access", "refresh_token":"dummy-refresh"}}).to_string()));
        archive.validate().unwrap();
        let result = archive
            .restore_with_session_support(&store, HashMap::new(), false)
            .unwrap();
        assert!(result.chatgpt_session_skipped);
        assert_eq!(store.snapshot().instructions, "Restored instructions");
        assert!(
            store
                .snapshot()
                .providers
                .iter()
                .any(|provider| provider.kind == ProviderKind::ChatGptAccount)
        );
    }
}
