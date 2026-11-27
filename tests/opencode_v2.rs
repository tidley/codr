use herdr_white_noise_bridge::output::final_messages_v2;
use serde_json::json;

#[test]
fn v2_explicit_parent_preserves_an_earlier_turn_during_steering() {
    let export = json!({"messages":[
        {"type":"user","id":"msg_first","text":"first"},
        {"type":"user","id":"msg_second","text":"second"},
        {"type":"assistant","id":"msg_answer","parentID":"msg_first","finish":"stop",
            "time":{"completed":3},"content":[{"type":"text","text":"First answer"}]}
    ]});
    let replies = final_messages_v2(&export).unwrap();
    assert_eq!(replies[0].turn.as_deref(), Some("opencode:msg_first"));
}

#[tokio::test]
async fn v2_pending_question_is_read_from_live_forms_not_the_completed_export() {
    use herdr_white_noise_bridge::{BridgeConfig, herdr::Session, output};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let cli = root.path().join("opencode");
    std::fs::write(
        &cli,
        r#"#!/bin/sh
cd "$(dirname "$0")" || exit 2
case "$1" in
  --pure) exit 2 ;;
  session) printf '%s' '{"messages":[{"type":"user","id":"user","text":"prompt"}]}' ;;
  api) [ "$2" = get ] && [ "$3" = /api/session/ses_fixture/form ] || exit 2
       cat forms.json ;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let forms = root.path().join("forms.json");
    std::fs::write(&forms, json!({"data":[{
        "id":"frm_question", "sessionID":"ses_fixture", "title":"Questions",
        "metadata":{"kind":"question","tool":{"messageID":"msg_pending","id":"call_pending"}},
        "fields":[{"key":"q0","title":"Scope","description":"Which next step?","type":"string",
            "options":[{"value":"Proceed","label":"Proceed","description":"Continue"},
                       {"value":"Review","label":"Review","description":"Review first"}],"custom":true}]
    }]}).to_string()).unwrap();
    let config: BridgeConfig = serde_json::from_value(json!({
        "phone_npub":"unused", "account_id_hex":"aa".repeat(32),
        "state_path":root.path().join("state.json"), "herdr_socket":root.path().join("herdr.sock"),
        "wn_agent_socket":root.path().join("mdk.sock"), "opencode_bin":cli
    }))
    .unwrap();
    let session = Session {
        source: "herdr:opencode".into(),
        agent: "opencode".into(),
        kind: "id".into(),
        value: "ses_fixture".into(),
    };
    let snapshot = output::read_snapshot(&config, &session).await.unwrap();
    assert!(snapshot.history.finals.is_empty());
    let decision = snapshot
        .decision
        .expect("The live question is absent from V2 exports");
    assert_eq!(decision.id, "msg_pending:call_pending");
    assert!(decision.supported());
    assert_eq!(decision.choice("Review").unwrap(), 2);
    assert!(decision.matches_visible("Which next step?\n1. Proceed\nContinue\n2. Review\nReview first\n3. Type your own answer\n↑↓ select enter submit esc dismiss"));
    assert_eq!(
        output::decision::read(&config, &session)
            .await
            .unwrap()
            .unwrap()
            .id,
        decision.id
    );
    std::fs::write(&forms, r#"{"data":[]}"#).unwrap();
    assert!(
        output::decision::read(&config, &session)
            .await
            .unwrap()
            .is_none()
    );
    std::fs::remove_file(&forms).unwrap();
    let snapshot = output::read_snapshot(&config, &session).await.unwrap();
    assert!(
        snapshot.decision.is_none(),
        "A failed live read cannot reuse a stale question"
    );
    assert_eq!(
        snapshot.history.user_inputs.len(),
        1,
        "Question API failures do not block history reads"
    );
}

#[test]
fn v2_export_keeps_only_completed_replies_from_stopped_turns() {
    let export = json!({
        "info": {"id": "ses_fixture"},
        "messages": [
            {"type":"user","id":"msg_u1","text":"first","time":{"created":1},"agents":[],"files":[]},
            {"type":"assistant","id":"msg_a1","agent":"build","finish":"tool-calls",
             "time":{"created":2,"completed":3},
             "content":[{"type":"text","text":"intermediate"},
                        {"type":"tool","id":"tool_1","name":"shell","state":{"status":"completed","input":{}}}]},
            {"type":"assistant","id":"msg_a2","agent":"build","finish":"stop",
             "time":{"created":4,"completed":5},
             "content":[{"type":"reasoning","text":"thinking"},{"type":"text","text":"First answer"}]},
            {"type":"synthetic","id":"msg_s1","text":"<subagent/>","description":"child",
             "metadata":{"childID":"ses_child"},"time":{"created":6}},
            {"type":"idle","id":"msg_i1","outcome":"succeeded","time":{"created":7}},
            {"type":"user","id":"msg_u2","text":"second","time":{"created":8},"agents":[],"files":[]},
            {"type":"assistant","id":"msg_a3","agent":"build","finish":"stop",
             "time":{"created":9,"completed":10},
             "content":[{"type":"text","text":"Second"},{"type":"text","text":"answer"}]},
            {"type":"assistant","id":"msg_a4","agent":"build","finish":"stop",
             "time":{"created":11},"content":[{"type":"text","text":"unfinished"}]},
            {"type":"assistant","id":"msg_a5","agent":"build","finish":"error",
             "time":{"created":12,"completed":13},"error":{"message":"failed"},
             "content":[{"type":"text","text":"failed turn"}]}
        ]
    });
    let finals = final_messages_v2(&export).unwrap();
    assert_eq!(finals.len(), 2);
    assert_eq!(finals[0].text, "First answer");
    assert_eq!(finals[0].turn.as_deref(), Some("opencode:msg_u1"));
    assert_eq!(finals[1].text, "Second\nanswer");
    assert_eq!(finals[1].turn.as_deref(), Some("opencode:msg_u2"));
}

#[test]
fn v2_export_without_conversation_messages_has_no_finals() {
    let export = json!({
        "info": {"id": "ses_empty"},
        "messages": [
            {"type":"synthetic","id":"msg_s1","text":"<subagent/>","metadata":{},"time":{"created":1}},
            {"type":"idle","id":"msg_i1","outcome":"succeeded","time":{"created":2}}
        ]
    });
    assert!(final_messages_v2(&export).unwrap().is_empty());
}
