//! Transport fixtures exercise the account bridge without real credentials.
#[cfg(unix)]
#[tokio::test]
async fn fatal_tool_callback_stops_codex_before_later_requests_are_executed() {
    use opendex::{
        codex::Bridge,
        providers::Tool,
        state::{Message, Role},
    };
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    let script = folder.path().join("codex-fixture");
    std::fs::write(&script,r#"#!/usr/bin/env python3
import json,sys
def send(value): print(json.dumps(value),flush=True)
for line in sys.stdin:
    request=json.loads(line)
    method=request.get('method')
    if method=='initialize': result={}
    elif method=='account/read': result={'account':{'type':'chatgpt','email':'fixture@example.org'}}
    elif method=='thread/start':
        assert request['params']['dynamicTools'][0]['name']=='fixture_tool'
        result={'thread':{'id':'fixture-thread'}}
    elif method=='turn/start':
        send({'id':request['id'],'result':{}})
        send({'id':'tool1','method':'item/tool/call','params':{'threadId':'fixture-thread','tool':'fixture_tool','callId':'call1','arguments':{}}})
        response=json.loads(sys.stdin.readline())
        assert response['result']['success'] is True
        # Queue later requests to prove an error stops the read loop immediately.
        for i in [2,3]:send({'id':'tool'+str(i),'method':'item/tool/call','params':{'threadId':'fixture-thread','tool':'fixture_tool','callId':'call'+str(i),'arguments':{}}})
        sys.stdin.read()
        break
    else:continue
    send({'id':request['id'],'result':result})
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::start(script.to_str().unwrap(), &folder.path().join("codex"))
        .await
        .unwrap();
    let mut calls = 0;
    let error = bridge
        .generate(
            "fixture-model",
            "",
            &[Message::new(Role::User, "Use the fixture tool")],
            &[Tool {
                name: "fixture_tool".into(),
                description: "Fixture".into(),
                schema: json!({"type":"object"}),
            }],
            |_| {},
            |_| {
                calls += 1;
                let count = calls;
                async move {
                    anyhow::ensure!(count <= 1, "Tool budget exhausted");
                    Ok((true, "First call finished".into()))
                }
            },
        )
        .await
        .unwrap_err();
    assert_eq!(calls, 2);
    assert!(error.to_string().contains("Tool budget exhausted"));
}
