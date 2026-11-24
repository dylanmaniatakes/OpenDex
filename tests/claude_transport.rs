//! No subscription credentials or inference calls are used by these checks.
use opendex::claude::Bridge;

#[cfg(unix)]
async fn fixture(body: &str) -> anyhow::Result<Bridge> {
    fixture_turn(body, &[]).await
}

#[cfg(unix)]
async fn fixture_turn(body: &str, tools: &[opendex::providers::Tool]) -> anyhow::Result<Bridge> {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir()?;
    let script = folder.path().join("claude-fixture");
    std::fs::write(&script, format!("#!/usr/bin/env python3\n{body}"))?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    // The child has read its executable before start returns. Its working and
    // configuration folders are never the fixture's executable directory.
    Bridge::start_turn(
        script.to_str().unwrap(),
        &folder.path().join("account"),
        Some("sonnet"),
        "Test instructions",
        tools,
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn initializes_isolated_cli_and_rejects_unexpected_native_operations() {
    let bridge = fixture(r#"
import json, os, sys
args = sys.argv[1:]
for flag, value in [('--tools', ''), ('--setting-sources', ''), ('--input-format', 'stream-json'), ('--output-format', 'stream-json')]:
    assert args[args.index(flag)+1] == value
assert '--strict-mcp-config' in args and '--disable-slash-commands' in args
assert '--no-session-persistence' in args
assert json.loads(args[args.index('--mcp-config')+1]) == {'mcpServers':{}}
assert json.loads(args[args.index('--settings')+1])['disableAllHooks'] is True
assert os.path.isabs(os.environ['CLAUDE_CONFIG_DIR'])
assert os.getcwd() != os.environ['CLAUDE_CONFIG_DIR']
assert not any(key.startswith('ANTHROPIC_') for key in os.environ)
request = json.loads(sys.stdin.readline())
assert request['request']['subtype'] == 'initialize'
assert request['request']['skills'] == []
print(json.dumps({'type':'control_request','request_id':'native-operation','request':{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'touch forbidden'}}}), flush=True)
reply = json.loads(sys.stdin.readline())
assert reply['response']['request_id'] == 'native-operation'
assert reply['response']['subtype'] == 'error'
# Ignore responses to other requests without mistaking them for initialization.
print(json.dumps({'type':'control_response','response':{'request_id':'unrelated','subtype':'success','response':{}}}), flush=True)
print(json.dumps({'type':'control_response','response':{'request_id':request['request_id'],'subtype':'success','response':{'models':[{'value':'sonnet','displayName':'Sonnet'}]}}}), flush=True)
# Wait for OpenDex to drop the child. No user prompt is expected.
sys.stdin.read()
"#).await.unwrap();
    assert_eq!(bridge.initialization()["models"][0]["value"], "sonnet");
}

#[cfg(unix)]
#[tokio::test]
async fn malformed_or_failed_initialization_is_actionable() {
    for body in [
        "print('not JSON', flush=True)",
        "print('{\"type\":\"result\",\"is_error\":true}', flush=True)",
        "print('{\"type\":\"control_response\",\"response\":{\"request_id\":\"opendex-initialize\",\"subtype\":\"error\"}}', flush=True)",
    ] {
        assert!(fixture(body).await.is_err());
    }
}

#[tokio::test]
#[ignore = "requires the installed, unmodified Claude Code CLI"]
async fn installed_claude_initializes_without_an_inference_prompt() {
    let folder = tempfile::tempdir().unwrap();
    let program = std::env::var("OPENDEX_TEST_CLAUDE").unwrap_or_else(|_| "claude".into());
    let bridge = Bridge::start_turn(
        &program,
        &folder.path().join("claude"),
        None,
        "Test initialization only.",
        &[opendex::providers::Tool {
            name: "fixture_echo".into(),
            description: "Unused initialization fixture".into(),
            schema: serde_json::json!({"type":"object"}),
        }],
    )
    .await
    .unwrap();
    assert!(
        bridge.initialization()["models"]
            .as_array()
            .is_some_and(|models| !models.is_empty())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn streams_chat_and_routes_only_exposed_tools_through_callback() {
    use opendex::{
        providers::Tool,
        state::{Attachment, Message, Role},
    };
    use serde_json::json;
    let tools = [Tool {
        name: "read_file".into(),
        description: "Read project file".into(),
        schema: json!({"type":"object"}),
    }];
    let mut bridge = fixture_turn(r#"
import json,sys
args=sys.argv[1:]
assert args[args.index('--model')+1]=='sonnet'
assert args[args.index('--system-prompt')+1]=='Test instructions'
assert args[args.index('--allowedTools')+1]=='mcp__opendex__read_file'
assert json.loads(args[args.index('--mcp-config')+1])['mcpServers']['opendex']['type']=='sdk'
def send(value): print(json.dumps(value),flush=True)
def control(method,params={}):
    send({'type':'control_request','request_id':method,'request':{'subtype':'mcp_message','server_name':'opendex','message':{'id':17,'method':method,'params':params}}})
    return json.loads(sys.stdin.readline())['response']['response']['mcp_response']
request=json.loads(sys.stdin.readline())
assert control('initialize',{'protocolVersion':'2025-11-25'})['result']['capabilities']=={'tools':{}}
assert control('tools/list')['result']['tools'][0]['name']=='read_file'
send({'type':'control_response','response':{'request_id':request['request_id'],'subtype':'success','response':{'models':[]}}})
user=json.loads(sys.stdin.readline())
assert user['client_composed'] is True
assert 'earlier question' in user['message']['content'][0]['text']
assert user['message']['content'][1]['type']=='image'
assert user['message']['content'][2]['type']=='document'
assert control('tools/call',{'name':'hidden','arguments':{}})['result']['isError'] is True
output=control('tools/call',{'name':'read_file','arguments':{'path':'main.rs'}})['result']
assert output['isError'] is True and output['content'][0]['text']=='Permission denied'
send({'type':'control_request','request_id':'native-during-turn','request':{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'touch forbidden'}}})
assert json.loads(sys.stdin.readline())['response']['subtype']=='error'
send({'type':'stream_event','event':{'delta':{'type':'text_delta','text':'Done'}}})
send({'type':'assistant','message':{'content':[{'type':'text','text':'Done'}]}})
# Unstreamed assistant text after a tool cycle must still be shown once.
send({'type':'assistant','message':{'content':[{'type':'text','text':' safely'}]}})
send({'type':'result','subtype':'success','is_error':False,'result':'Done safely'})
sys.stdin.read()
"#, &tools).await.unwrap();
    let mut prompt = Message::new(Role::User, "latest question");
    prompt.attachments = vec![
        Attachment {
            name: "pic.png".into(),
            text: "AA==".into(),
            media_type: Some("image/png".into()),
        },
        Attachment {
            name: "doc.pdf".into(),
            text: "AA==".into(),
            media_type: Some("application/pdf".into()),
        },
    ];
    let mut streamed = String::new();
    let mut calls = Vec::new();
    let result = bridge
        .generate(
            &[Message::new(Role::User, "earlier question"), prompt],
            |piece| streamed.push_str(piece),
            |call| {
                calls.push(call);
                async { (false, "Permission denied".into()) }
            },
        )
        .await
        .unwrap();
    assert_eq!(result, "Done safely");
    assert_eq!(streamed, result);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments["path"], "main.rs");
}
