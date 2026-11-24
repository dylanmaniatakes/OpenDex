use opendex::{
    agent,
    state::{McpScope, Mode, Permissions, Profile, Rule, Server, Store},
};
use serde_json::json;
use std::{fs, net::TcpListener};
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn file_attachments_keep_order_and_enforce_a_combined_limit() {
    let directory = tempdir().unwrap();
    let text = directory.path().join("notes.txt");
    let image = directory.path().join("graph.png");
    fs::write(&text, "Notes for the graph").unwrap();
    fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let files = agent::load_attachments(vec![text.clone(), image.clone(), text.clone()]).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].1.text, "Notes for the graph");
    assert_eq!(files[1].1.media_type.as_deref(), Some("image/png"));
    assert!(agent::load_attachments(vec![text; 9]).is_err());

    let mut large_image = b"\x89PNG\r\n\x1a\n".to_vec();
    large_image.resize(5 * 1024 * 1024, 0);
    let second = directory.path().join("second.png");
    let third = directory.path().join("third.png");
    for path in [&image, &second, &third] {
        fs::write(path, &large_image).unwrap();
    }
    let error = agent::load_attachments(vec![image, second, third]).unwrap_err();
    assert!(error.to_string().contains("12 MiB"));
}

#[test]
fn mcp_tool_rules_only_narrow_the_profile_and_preserve_legacy_blocks() {
    let mut server: Server = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "name": "Fixture", "transport": "http",
        "address": "http://localhost/mcp", "blocked_tools": ["legacy"]
    }))
    .unwrap();
    assert_eq!(server.tool_rule(Rule::Allow, "legacy"), Rule::Deny);
    assert_eq!(server.tool_rule(Rule::Allow, "inherited"), Rule::Allow);

    server.tool_permission = Some(Rule::Ask);
    server.tool_permissions.insert("allow".into(), Rule::Allow);
    server.tool_permissions.insert("deny".into(), Rule::Deny);
    assert_eq!(server.tool_rule(Rule::Allow, "inherited"), Rule::Ask);
    assert_eq!(server.tool_rule(Rule::Allow, "allow"), Rule::Ask);
    assert_eq!(server.tool_rule(Rule::Allow, "deny"), Rule::Deny);
    assert_eq!(server.tool_rule(Rule::Deny, "allow"), Rule::Deny);
    server.tool_permission = Some(Rule::Deny);
    assert_eq!(server.tool_rule(Rule::Allow, "allow"), Rule::Deny);

    server.tool_permission = Some(Rule::Allow);
    server.tool_permissions.insert("ask".into(), Rule::Ask);
    assert_eq!(server.tool_rule(Rule::Allow, "ask"), Rule::Ask);
    assert_eq!(server.tool_rule(Rule::Ask, "allow"), Rule::Ask);
}

#[test]
fn empty_mcp_selection_disables_servers_after_serialization() {
    let scope = McpScope::Selected(vec![]);
    let restored: McpScope = serde_json::from_value(serde_json::to_value(&scope).unwrap()).unwrap();
    assert_eq!(restored, scope);
    assert!(!restored.includes(Uuid::new_v4()));
}

#[test]
fn app_state_survives_reopen_and_rejects_overlapping_processes() {
    let directory = tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    store
        .change(|data| {
            data.instructions = "Use concise prose.".into();
            Ok(())
        })
        .unwrap();
    assert!(Store::open(directory.path()).is_err());
    drop(store);
    let reopened = Store::open(directory.path()).unwrap();
    assert_eq!(reopened.snapshot().instructions, "Use concise prose.");
}

#[tokio::test]
async fn edit_requires_a_unique_match_and_writes_stay_inside_selected_folder() {
    let folder = tempdir().unwrap();
    let root = agent::checked_root(folder.path()).unwrap();
    fs::write(root.join("example.txt"), "item\nitem\n").unwrap();
    let policy = Permissions::default().effective();
    let ambiguous = agent::execute(
        &root,
        &policy,
        "edit_file",
        &json!({"path":"example.txt", "old_text":"item", "new_text":"replacement"}),
    )
    .await;
    assert!(ambiguous.is_err());
    assert_eq!(
        fs::read_to_string(root.join("example.txt")).unwrap(),
        "item\nitem\n"
    );
    agent::execute(
        &root,
        &policy,
        "edit_file",
        &json!({"path":"example.txt", "old_text":"item\nitem", "new_text":"replacement"}),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("example.txt")).unwrap(),
        "replacement\n"
    );
    agent::execute(
        &root,
        &policy,
        "create_file",
        &json!({"path":"added.txt", "content":"new\n"}),
    )
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(root.join("added.txt")).unwrap(), "new\n");
    assert!(agent::workspace_path(&root, "../outside.txt", true).is_err());
    assert!(agent::workspace_path(&root, ".git/config", false).is_err());

    #[cfg(unix)]
    {
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("outside.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert!(agent::workspace_path(&root, "link/outside.txt", false).is_err());
    }
}

#[tokio::test]
async fn read_only_profile_hides_and_rejects_mutating_tools() {
    let folder = tempdir().unwrap();
    let root = agent::checked_root(folder.path()).unwrap();
    let policy = Permissions {
        profile: Profile::ReadOnly,
        write: Rule::Allow,
        ..Default::default()
    }
    .effective();
    let names = agent::tools(Mode::Agent, Some(&root), &policy)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"read_file".into()));
    assert!(!names.contains(&"run_command".into()));
    assert!(
        agent::execute(
            &root,
            &policy,
            "create_file",
            &json!({"path":"bad.txt","content":"no"})
        )
        .await
        .is_err()
    );
    assert!(!root.join("bad.txt").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn workspace_command_blocks_external_reads_writes_and_network() {
    let folder = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let root = agent::checked_root(folder.path()).unwrap();
    let external = outside.path().join("external.txt");
    fs::write(&external, "off-limits").unwrap();
    fs::write(root.join("input.txt"), "inside").unwrap();
    let nonce = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", nonce.local_addr().unwrap());
    let policy = Permissions::default().effective();
    let command =
        |script| json!({"program":"/bin/sh","args":["-c",script,"fixture", external,url]});
    let good = agent::execute(
        &root,
        &policy,
        "run_command",
        &command("cat input.txt && printf result > output.txt"),
    )
    .await
    .unwrap();
    assert!(good.starts_with("Exit 0\n"), "{good}");
    assert_eq!(
        fs::read_to_string(root.join("output.txt")).unwrap(),
        "result"
    );
    let read = agent::execute(&root, &policy, "run_command", &command("cat \"$1\""))
        .await
        .unwrap();
    assert!(read.contains("Operation not permitted"), "{read}");
    let write = agent::execute(
        &root,
        &policy,
        "run_command",
        &command("printf changed > \"$1\""),
    )
    .await
    .unwrap();
    assert!(write.contains("Operation not permitted"), "{write}");
    assert_eq!(fs::read_to_string(&external).unwrap(), "off-limits");
    let net = agent::execute(
        &root,
        &policy,
        "run_command",
        &command("/usr/bin/curl -sS --max-time 2 \"$2\""),
    )
    .await
    .unwrap();
    assert!(net.starts_with("Exit 7\n"), "{net}");
    nonce.set_nonblocking(true).unwrap();
    assert_eq!(
        nonce.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    let policy = Permissions {
        profile: Profile::Custom,
        read: Rule::Allow,
        write: Rule::Deny,
        command: Rule::Allow,
        ..Default::default()
    }
    .effective();
    let denied = agent::execute(
        &root,
        &policy,
        "run_command",
        &command("printf no > refused.txt"),
    )
    .await
    .unwrap();
    assert!(denied.contains("Operation not permitted"), "{denied}");
    assert!(!root.join("refused.txt").exists());
}

#[test]
fn legacy_agent_threads_are_grouped_by_project_and_persist_the_link() {
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("sample");
    fs::create_dir(&workspace).unwrap();
    let keys = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
    let old = json!({
        "threads": keys.map(|id| json!({"id":id,"mode":"agent","title":"Old thread","workspace":workspace}))
    });
    fs::write(directory.path().join("workspace.json"), old.to_string()).unwrap();

    let store = Store::open(directory.path()).unwrap();
    let snapshot = store.snapshot();
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].name, "sample");
    let project = snapshot.projects[0].id;
    assert!(
        snapshot
            .threads
            .iter()
            .all(|thread| thread.project_id == Some(project))
    );
    drop(store);
    assert_eq!(
        Store::open(directory.path()).unwrap().snapshot().projects[0].id,
        project
    );
}

#[tokio::test]
async fn git_review_sees_new_files_and_worktrees_start_from_head() {
    let folder = tempdir().unwrap();
    let root = folder.path().join("repo");
    fs::create_dir(&root).unwrap();
    let git = |argv: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(argv)
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    git(&["init", "-b", "main"]);
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&[
        "-c",
        "user.name=QA",
        "-c",
        "user.email=qa@example.test",
        "commit",
        "-m",
        "fixture",
    ]);
    fs::write(root.join("tracked.txt"), "working directory\n").unwrap();
    fs::write(root.join("new.txt"), "new file\n").unwrap();
    let changes = agent::diff(&root).await.unwrap();
    assert!(changes.contains("+working directory"), "{changes}");
    assert!(changes.contains("+new file"), "{changes}");
    let target = folder.path().join("worktree");
    let output = agent::create_worktree(&root, &target, "qa-sample")
        .await
        .unwrap();
    assert!(output.starts_with("Exit 0\n"), "{output}");
    assert_eq!(
        fs::read_to_string(target.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("tracked.txt")).unwrap(),
        "working directory\n"
    );
}

#[test]
fn concurrent_model_updates_preserve_connection_settings_and_other_models() {
    let directory = tempdir().unwrap();
    let store = std::sync::Arc::new(Store::open(directory.path()).unwrap());
    let id = store.snapshot().providers[0].id;
    store
        .change(|data| {
            data.providers[0].name = "Configured endpoint".into();
            data.providers[0].headers = vec![("x-organization".into(), "ORG".into())];
            data.add_provider_models(id, vec!["old-model".into()])?;
            Ok(())
        })
        .unwrap();
    let jobs: Vec<_> = ["first-model", "second-model", "old-model"]
        .into_iter()
        .map(|model| {
            let store = store.clone();
            std::thread::spawn(move || {
                store
                    .change(|data| {
                        if model == "old-model" {
                            data.remove_provider_model(id, model)
                        } else {
                            data.add_provider_models(id, vec![model.into(), model.into()])
                        }
                    })
                    .unwrap();
            })
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    let before = store.snapshot().providers[0].clone();
    let mut models = before.models.clone();
    models.sort();
    assert_eq!(models, ["first-model", "second-model"]);
    assert_eq!(before.name, "Configured endpoint");
    assert_eq!(before.headers, [("x-organization".into(), "ORG".into())]);
    assert!(
        store
            .change(|data| data.add_provider_models(id, vec!["invalid\nmodel".into()]))
            .is_err()
    );
    assert_eq!(store.snapshot().providers[0].models, before.models);
    drop(store);
    assert_eq!(
        Store::open(directory.path()).unwrap().snapshot().providers[0].models,
        before.models
    );
}

#[test]
fn mcp_http_header_rules_are_validated_and_managed_headers_locked() {
    use opendex::state::Transport;
    let mut server: Server = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "name": "Fixture", "transport": "http",
        "address": "http://localhost/mcp"
    }))
    .unwrap();
    server.transport = Transport::Http;
    server.headers = vec![("x-organization".to_string(), "ORG".to_string())];
    server.validate().unwrap();

    server
        .headers
        .push(("X-Organization".to_string(), "OTHER".to_string()));
    assert!(server.validate().is_err());

    server.headers = vec![("authorization".to_string(), "SECRET".to_string())];
    assert!(server.validate().is_err());

    server.headers = vec![("Mcp-Session-Id".to_string(), "X".to_string())];
    assert!(server.validate().is_err());

    server.transport = Transport::Stdio;
    server.headers = vec![("x-organization".to_string(), "ORG".to_string())];
    server.validate().unwrap();
    assert!(server.headers.is_empty());
}
