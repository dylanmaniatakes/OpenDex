use opendex::{
    backup,
    state::{McpScope, Message, Project, Provider, ProviderKind, Role, Rule, Store, Thread},
};
use serde_json::json;
use std::{collections::HashMap, fs};
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn encrypted_settings_require_authentication_and_restore_without_replacing_chats() {
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
            data.providers[0].models.push("pinned-model".into());
            data.providers.push(Provider {
                name: "Login metadata".into(),
                kind: ProviderKind::ChatGptAccount,
                endpoint: String::new(),
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
    backup::export(&source, &path, passphrase.into()).unwrap();
    let mut ciphertext = fs::read(&path).unwrap();
    assert!(ciphertext.starts_with(b"age-encryption.org/v1\n"));
    assert!(
        !ciphertext
            .windows(15)
            .any(|window| window == b"Backup endpoint")
    );
    assert!(backup::open(&path, "incorrect".into(), None).is_err());

    let (archive, checksum) = backup::open(&path, passphrase.into(), None).unwrap();
    let preview = archive.preview(checksum);
    assert_eq!(preview.providers, ["Backup endpoint", "Login metadata"]);
    assert!(preview.has_chatgpt_connection);
    assert_eq!(preview.mcp_servers, ["MCP fixture"]);
    assert!(!preview.projects[0].available);
    assert!(destination.snapshot().instructions.is_empty());
    let mapped = folder.path().join("mapped");
    fs::create_dir(&mapped).unwrap();
    archive
        .restore(&destination, HashMap::from([(project_id, mapped.clone())]))
        .unwrap();
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
    assert_eq!(
        restored.servers[0].tool_rule(Rule::Allow, "fixture"),
        Rule::Deny
    );
    assert_eq!(restored.appearance.theme, "light");
    assert_eq!(restored.instructions, "Archive fixture instructions");

    *ciphertext.last_mut().unwrap() ^= 1;
    fs::write(&path, ciphertext).unwrap();
    let error = backup::open(&path, passphrase.into(), Some(checksum))
        .err()
        .unwrap();
    assert!(error.to_string().contains("backup file changed"));
    assert!(backup::open(&path, passphrase.into(), None).is_err());
}
