use opendex::state::{
    Attachment, BranchAction, McpResource, McpScope, Message, Mode, Role, Store, Thread,
};
use serde_json::json;
use uuid::Uuid;

fn thread(messages: Vec<Message>) -> Thread {
    serde_json::from_value(json!({
        "id":Uuid::new_v4(), "mode":"chat", "title":"Example chat", "instructions":"Be concise.",
        "provider_id":Uuid::new_v4(), "model_id":"example", "messages":messages
    }))
    .unwrap()
}

#[test]
fn editing_keeps_attachments_and_leaves_the_source_intact() {
    let mut prompt = Message::new(Role::User, "Original question");
    prompt.attachments.push(Attachment {
        name: "context.txt".into(),
        text: "Saved file snapshot".into(),
        media_type: None,
    });
    prompt.attachments.push(Attachment {
        name: "diagram.png".into(),
        text: "fixture-image".into(),
        media_type: Some("image/png".into()),
    });
    prompt.resources.push(McpResource {
        server_id: Uuid::new_v4(),
        source: "Example server".into(),
        name: "Context".into(),
        uri: "notes://context".into(),
        text: "Saved resource snapshot".into(),
    });
    let id = prompt.id;
    let source = thread(vec![
        prompt,
        Message::new(Role::Assistant, "Original answer"),
        Message::new(Role::User, "A later question"),
    ]);
    let branch = source
        .branch_at(
            id,
            BranchAction::Edit {
                text: " Revised question ".into(),
            },
        )
        .unwrap();
    assert_eq!(branch.parent_thread, Some(source.id));
    assert_eq!(branch.provider_id, source.provider_id);
    assert_eq!(branch.model_id, source.model_id);
    assert_eq!(branch.instructions, source.instructions);
    assert_eq!(branch.messages.len(), 1);
    assert_ne!(branch.messages[0].id, id);
    assert_eq!(branch.messages[0].text, "Revised question");
    assert!(branch.messages[0].content().contains("Saved file snapshot"));
    assert_eq!(branch.messages[0].attachments.len(), 2);
    assert_eq!(branch.messages[0].attachments[1].text, "fixture-image");
    assert!(
        branch.messages[0]
            .content()
            .contains("Saved resource snapshot")
    );
    assert_eq!(source.messages.len(), 3);
    assert_eq!(source.messages[0].text, "Original question");
}

#[test]
fn legacy_single_attachments_load_and_serialize_as_a_list() {
    let message: Message = serde_json::from_value(json!({
        "role":"user", "text":"",
        "attachment":{"name":"legacy.md","text":"Preserved context"}
    }))
    .unwrap();
    assert!(message.has_content());
    assert_eq!(message.attachments[0].name, "legacy.md");
    let stored = serde_json::to_value(&message).unwrap();
    assert!(stored.get("attachment").is_none());
    assert_eq!(stored["attachments"][0]["text"], "Preserved context");
}

#[test]
fn retry_branches_at_the_prompt_for_the_selected_reply() {
    let earlier = Message::new(Role::Assistant, "Earlier reply");
    let anchor = earlier.id;
    let source = thread(vec![
        Message::new(Role::User, "Earlier prompt"),
        Message::new(Role::Assistant, "Looking it up"),
        Message::new(Role::Tool, "Result"),
        earlier,
        Message::new(Role::User, "Later prompt"),
        Message::new(Role::Assistant, "Later answer"),
    ]);
    let branch = source.branch_at(anchor, BranchAction::Retry).unwrap();
    assert_eq!(branch.messages.len(), 1);
    assert_eq!(branch.messages[0].text, "Earlier prompt");
    assert_eq!(branch.messages[0].id, source.messages[0].id);
    assert_eq!(source.messages[5].text, "Later answer");
    assert!(
        source
            .branch_at(source.messages[2].id, BranchAction::Continue)
            .is_err()
    );
    assert!(
        source
            .branch_at(Uuid::new_v4(), BranchAction::Continue)
            .is_err()
    );
    assert!(
        source
            .branch_at(
                source.messages[0].id,
                BranchAction::Edit { text: "  ".into() }
            )
            .is_err()
    );
}

#[test]
fn code_agent_branch_preserves_project_and_history_at_the_selected_message() {
    let mut source = thread(vec![
        Message::new(Role::User, "Inspect"),
        Message::new(Role::Tool, "File listing"),
        Message::new(Role::Assistant, "Inspection complete"),
        Message::new(Role::User, "Later change"),
    ]);
    source.mode = Mode::Agent;
    source.project_id = Some(Uuid::new_v4());
    source.workspace = Some("/tmp/example-worktree".into());
    let server = Uuid::new_v4();
    assert!(source.mcp_scope.includes(server));
    source.mcp_scope = McpScope::Selected(vec![server]);
    let branch = source
        .branch_at(source.messages[2].id, BranchAction::Continue)
        .unwrap();
    assert_eq!(branch.workspace, source.workspace);
    assert_eq!(branch.project_id, source.project_id);
    assert_eq!(branch.mcp_scope, source.mcp_scope);
    assert!(!branch.mcp_scope.includes(Uuid::new_v4()));
    assert_eq!(branch.messages.len(), 3);
    assert_eq!(branch.messages[1].role, Role::Tool);
    assert!(
        source
            .branch_at(source.messages[2].id, BranchAction::Retry)
            .is_err()
    );

    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    store
        .change(|data| {
            data.threads.push(source.clone());
            data.threads.push(branch.clone());
            data.active_thread = Some(branch.id);
            Ok(())
        })
        .unwrap();
    drop(store);
    let persisted = Store::open(directory.path()).unwrap().snapshot();
    assert_eq!(persisted.active_thread, Some(branch.id));
    assert_eq!(persisted.threads[1].parent_thread, Some(source.id));
    assert_eq!(
        persisted.threads[1].mcp_scope,
        McpScope::Selected(vec![server])
    );
    assert_eq!(persisted.threads[0].messages.len(), 4);
}
