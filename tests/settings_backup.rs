use opendex::{
    backup, images,
    state::{
        AnthropicAuth, McpScope, Message, Project, Provider, ProviderKind, Role, Rule, Store,
        Thread, read_secret, write_secret,
    },
    web::Config,
};
use serde_json::json;
use std::{
    any::Any,
    collections::HashMap,
    fs,
    sync::{Arc, Mutex},
};
use tempfile::tempdir;
use uuid::Uuid;

// Shared in-memory credentials exercise export and restore without touching
// the developer's OS credential store. Each Entry resolves the same saved slot.
#[derive(Default)]
struct TestKeyring(Arc<Mutex<HashMap<String, Vec<u8>>>>);
struct TestCredential {
    entries: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    slot: String,
}
impl keyring::credential::CredentialBuilderApi for TestKeyring {
    fn build(
        &self,
        target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<keyring::credential::Credential>> {
        Ok(Box::new(TestCredential {
            entries: self.0.clone(),
            slot: format!("{target:?}:{service}:{user}"),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl keyring::credential::CredentialApi for TestCredential {
    fn set_secret(&self, value: &[u8]) -> keyring::Result<()> {
        self.entries
            .lock()
            .unwrap()
            .insert(self.slot.clone(), value.to_vec());
        Ok(())
    }
    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        self.entries
            .lock()
            .unwrap()
            .get(&self.slot)
            .cloned()
            .ok_or(keyring::Error::NoEntry)
    }
    fn delete_credential(&self) -> keyring::Result<()> {
        self.entries
            .lock()
            .unwrap()
            .remove(&self.slot)
            .map(|_| ())
            .ok_or(keyring::Error::NoEntry)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test]
fn encrypted_settings_require_authentication_and_restore_without_replacing_chats() {
    keyring::set_default_credential_builder(Box::new(TestKeyring::default()));
    let folder = tempdir().unwrap();
    let source = Store::open(&folder.path().join("source")).unwrap();
    let destination = Store::open(&folder.path().join("destination")).unwrap();
    let path = folder.path().join("OpenDex-settings.age");
    let passphrase = "fixture settings passphrase";
    let project_id = Uuid::new_v4();
    let thread_id = Uuid::new_v4();
    let server_id = Uuid::new_v4();
    source
        .change(|data| {
            data.providers[0].name = "Backup endpoint".into();
            data.providers[0].kind = ProviderKind::OpenAiCompatible;
            data.providers[0].models.push("pinned-model".into());
            data.image_generation = Some(images::Config {
                provider: data.providers[0].id,
                model: "image-fixture".into(),
                size: Some("1024x1024".into()),
                quality: Some("medium".into()),
                request_base64: false,
            });
            data.providers.push(Provider {
                name: "Login metadata".into(),
                kind: ProviderKind::ChatGptAccount,
                endpoint: String::new(),
                ..Provider::local()
            });
            data.providers.push(Provider {
                name: "Anthropic account".into(),
                kind: ProviderKind::Anthropic,
                anthropic_auth: AnthropicAuth::Account,
                endpoint: "https://api.anthropic.com/v1".into(),
                ..Provider::local()
            });
            data.servers.push(serde_json::from_value(json!({
                "id":server_id, "name":"MCP fixture", "transport":"http",
                "address":"http://localhost/mcp", "enabled":true,
                "tool_permissions":{"fixture":"deny"}
            }))?);
            data.projects.push(Project {
                id: project_id,
                name: "Portable project".into(),
                root: folder.path().join("absent"),
            });
            data.appearance.theme = "light".into();
            data.web_search = Some(Config::Searxng {
                endpoint: "http://localhost:8080/search".into(),
            });
            data.permissions.web = Rule::Allow;
            data.instructions = "Archive fixture instructions".into();
            let thread: Thread = serde_json::from_value(json!({
                "id":Uuid::new_v4(), "mode":"chat", "title":"Source chat",
                "messages":[Message::new(Role::User, "Private source conversation")]
            }))?;
            data.threads.push(thread);
            Ok(())
        })
        .unwrap();
    let source_provider = source.snapshot().providers[0].id;
    destination
        .change(|data| {
            let thread: Thread = serde_json::from_value(json!({
                "id":thread_id, "mode":"chat", "title":"Destination chat",
                "provider_id":source_provider, "model_id":"missing-model",
                "mcp_scope":{"mode":"selected","servers":[server_id,Uuid::new_v4()]}
            }))?;
            data.threads.push(thread);
            data.active_thread = Some(thread_id);
            Ok(())
        })
        .unwrap();
    let session = json!({"access_token":"fixture-access", "refresh_token":"fixture-refresh", "expires_at_ms":1}).to_string();
    source
        .change(|data| {
            for (index, secret) in [(0, "fixture-api-key"), (2, session.as_str())] {
                let provider = &mut data.providers[index];
                write_secret(provider.credential_id(), &provider.endpoint, secret)?;
                provider.has_secret = true;
            }
            let server = &mut data.servers[0];
            write_secret(server.credential_id(), &server.address, "fixture-mcp-key")?;
            server.has_secret = true;
            Ok(())
        })
        .unwrap();
    let chatgpt_session = json!({"tokens":{"access_token":"chatgpt-access-fixture","refresh_token":"chatgpt-refresh-fixture","account_id":"fixture-account"},"OPENAI_API_KEY":null}).to_string();
    opendex::codex::credential_entry(&source.codex_home())
        .unwrap()
        .set_password(&chatgpt_session)
        .unwrap();
    let previous_session = json!({"tokens":{"access_token":"destination-access","refresh_token":"destination-refresh"}}).to_string();
    opendex::codex::credential_entry(&destination.codex_home())
        .unwrap()
        .set_password(&previous_session)
        .unwrap();
    backup::export(&source, &path, passphrase.into()).unwrap();
    let mut ciphertext = fs::read(&path).unwrap();
    assert!(ciphertext.starts_with(b"age-encryption.org/v1\n"));
    for secret in [
        "fixture-api-key",
        "fixture-access",
        "fixture-refresh",
        "fixture-mcp-key",
        "chatgpt-access-fixture",
        "chatgpt-refresh-fixture",
    ] {
        assert!(
            !ciphertext
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        );
    }

    assert!(
        !ciphertext
            .windows(15)
            .any(|window| window == b"Backup endpoint")
    );
    assert!(backup::open(&path, "incorrect".into(), None).is_err());

    let (archive, checksum) = backup::open(&path, passphrase.into(), None).unwrap();
    let mut legacy = serde_json::to_value(&archive).unwrap();
    legacy.as_object_mut().unwrap().remove("chatgpt_session");
    let legacy: backup::Archive = serde_json::from_value(legacy).unwrap();
    assert!(!legacy.preview(checksum).has_chatgpt_session);
    let preview = archive.preview(checksum);
    assert_eq!(
        preview.providers,
        ["Backup endpoint", "Login metadata", "Anthropic account"]
    );
    assert!(preview.has_chatgpt_connection);
    assert!(preview.has_chatgpt_session);
    assert_eq!(preview.mcp_servers, ["MCP fixture"]);
    assert_eq!(preview.web_search, Some("SearXNG"));
    assert_eq!(preview.image_model.as_deref(), Some("image-fixture"));
    assert!(!preview.projects[0].available);
    assert!(destination.snapshot().instructions.is_empty());
    let failing = Store::open(&folder.path().join("failing")).unwrap();
    let failing_entry = opendex::codex::credential_entry(&failing.codex_home()).unwrap();
    failing_entry.set_password(&previous_session).unwrap();
    let failing_path = folder.path().join("failing/workspace.json");
    fs::rename(&failing_path, failing_path.with_extension("original")).unwrap();
    fs::create_dir(&failing_path).unwrap();
    let copy: backup::Archive =
        serde_json::from_value(serde_json::to_value(&archive).unwrap()).unwrap();
    assert!(copy.restore(&failing, HashMap::new()).is_err());
    assert_eq!(failing_entry.get_password().unwrap(), previous_session);
    let mapped = folder.path().join("mapped");
    fs::create_dir(&mapped).unwrap();
    archive
        .restore(&destination, HashMap::from([(project_id, mapped.clone())]))
        .unwrap();
    assert_eq!(
        opendex::codex::credential_entry(&destination.codex_home())
            .unwrap()
            .get_password()
            .unwrap(),
        chatgpt_session
    );
    assert_eq!(
        opendex::codex::credential_entry(&source.codex_home())
            .unwrap()
            .get_password()
            .unwrap(),
        chatgpt_session
    );
    let restored = destination.snapshot();
    assert_eq!(restored.projects[0].root, mapped.canonicalize().unwrap());
    assert_eq!(restored.threads.len(), 1);
    assert_eq!(restored.threads[0].id, thread_id);
    assert_eq!(restored.threads[0].provider_id, Some(source_provider));
    assert_eq!(restored.threads[0].model_id, None);
    assert_eq!(
        restored.threads[0].mcp_scope,
        McpScope::Selected(vec![server_id])
    );
    assert_eq!(restored.active_thread, Some(thread_id));
    assert_eq!(restored.providers[0].models, ["pinned-model"]);
    for (index, expected) in [(0, "fixture-api-key"), (2, session.as_str())] {
        let provider = &restored.providers[index];
        assert!(provider.has_secret);
        assert!(provider.secret_ref.is_some());
        assert_ne!(
            provider.credential_id(),
            source.snapshot().providers[index].credential_id()
        );
        assert_eq!(
            read_secret(provider.credential_id(), &provider.endpoint, true)
                .unwrap()
                .as_deref(),
            Some(expected)
        );
    }
    let server = &restored.servers[0];
    assert_eq!(
        read_secret(server.credential_id(), &server.address, true)
            .unwrap()
            .as_deref(),
        Some("fixture-mcp-key")
    );

    assert_eq!(restored.providers[2].kind, ProviderKind::Anthropic);
    assert_eq!(restored.providers[2].anthropic_auth, AnthropicAuth::Account);
    assert_eq!(
        restored.servers[0].tool_rule(Rule::Allow, "fixture"),
        Rule::Deny
    );
    assert_eq!(restored.appearance.theme, "light");
    assert_eq!(restored.web_search, source.snapshot().web_search);
    assert_eq!(
        restored.image_generation,
        source.snapshot().image_generation
    );
    assert_eq!(restored.permissions.web, Rule::Allow);
    assert_eq!(restored.instructions, "Archive fixture instructions");

    *ciphertext.last_mut().unwrap() ^= 1;
    fs::write(&path, ciphertext).unwrap();
    let error = backup::open(&path, passphrase.into(), Some(checksum))
        .err()
        .unwrap();
    assert!(error.to_string().contains("backup file changed"));
    assert!(backup::open(&path, passphrase.into(), None).is_err());
}
