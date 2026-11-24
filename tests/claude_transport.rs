//! No subscription credentials or inference calls are used by these checks.
use opendex::claude::Bridge;

#[cfg(unix)]
async fn fixture(body: &str) -> anyhow::Result<Bridge> {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir()?;
    let script = folder.path().join("claude-fixture");
    std::fs::write(&script, format!("#!/usr/bin/env python3\n{body}"))?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    // The child has read its executable before start returns. Its working and
    // configuration folders are never the fixture's executable directory.
    Bridge::start(script.to_str().unwrap(), &folder.path().join("account")).await
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
    let bridge = Bridge::start(&program, &folder.path().join("claude"))
        .await
        .unwrap();
    assert!(
        bridge.initialization()["models"]
            .as_array()
            .is_some_and(|models| !models.is_empty())
    );
}
