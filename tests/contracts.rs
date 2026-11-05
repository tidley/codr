use bech32::ToBase32;
use herdr_white_noise_bridge::{config::phone_hex, herdr::Agent, output::final_messages};
use serde_json::json;

#[test]
fn status_labels_distinguish_progress_from_settled_states() {
    for (status, expected) in [
        ("working", "Working..."),
        ("blocked", "Blocked..."),
        ("done", "Done."),
        ("idle", "Idle."),
        ("ended", "Ended."),
        ("unknown", "Unknown."),
    ] {
        assert_eq!(
            herdr_white_noise_bridge::herdr::status_label(status),
            expected
        );
    }
}

#[test]
fn npub_checksum_is_verified_and_decodes_a_synthetic_public_key() {
    let key = [0x11u8; 32];
    let npub = bech32::encode("npub", key.to_base32(), bech32::Variant::Bech32).unwrap();
    assert_eq!(phone_hex(&npub).unwrap(), hex::encode(key));
    let mut corrupted = npub.into_bytes();
    let last = corrupted.last_mut().unwrap();
    *last = if *last == b'q' { b'p' } else { b'q' };
    assert!(phone_hex(std::str::from_utf8(&corrupted).unwrap()).is_err());
}

#[test]
fn native_session_key_survives_pane_move_but_not_session_replacement() {
    let mut agent: Agent = serde_json::from_value(json!({"pane_id":"w1:p1", "terminal_id":"term1", "agent":"opencode", "agent_status":"idle", "agent_session":{"source":"herdr:opencode","agent":"opencode","kind":"id","value":"ses_1"}})).unwrap();
    let key = agent.key();
    agent.pane_id = "w2:p9".into();
    assert_eq!(agent.key(), key);
    agent.agent_session.as_mut().unwrap().value = "ses_2".into();
    assert_ne!(agent.key(), key);
}

#[test]
fn final_reply_filter_excludes_tools_reasoning_and_incomplete_turns() {
    let export = json!({"messages":[
        {"info":{"id":"u1","role":"user"},"parts":[{"type":"text","text":"private prompt"}]},
        {"info":{"id":"a1","role":"assistant","time":{"completed":1},"finish":"tool-calls"},"parts":[{"type":"text","text":"intermediate"}]},
        {"info":{"id":"a2","role":"assistant","parentID":"u1","time":{"completed":2},"finish":"stop"},"parts":[{"type":"reasoning","text":"private reasoning"},{"type":"text","text":"Final answer"}]},
        {"info":{"id":"a3","role":"assistant","finish":"stop"},"parts":[{"type":"text","text":"incomplete"}]}
    ]});
    let finals = final_messages(&export).unwrap();
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0].id, "a2");
    assert_eq!(finals[0].text, "Final answer");
}

#[test]
fn herdr_agent_detection_may_omit_its_backend_label() {
    let a: Agent = serde_json::from_value(
        json!({"pane_id":"w1:p1","terminal_id":"term1","agent_status":"unknown"}),
    )
    .unwrap();
    assert!(a.agent.is_empty());
}
