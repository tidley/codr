use bech32::ToBase32;
use herdr_white_noise_bridge::{
    BridgeConfig, BridgeState,
    bridge::{Bridge, now},
    herdr::Agent,
    state::Mapping,
    wire::{Socket, read_line},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use tokio::{io::AsyncWriteExt, net::UnixListener};

// Synthetic test identity. Never use it as an installation owner.
const HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn phone_npub() -> String {
    bech32::encode("npub", [0x11u8; 32].to_base32(), bech32::Variant::Bech32).unwrap()
}

fn config(dir: &std::path::Path) -> BridgeConfig {
    // tempfile's macOS root sits behind the /var symlink; launch roots must be real paths.
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    BridgeConfig {
        phone_npub: phone_npub(),
        herdr_socket: dir.join("herdr.sock"),
        wn_agent_socket: dir.join("wn.sock"),
        account_id_hex: "aa".repeat(32),
        state_path: dir.join("state.json"),
        launch_root: dir.join("projects"),
        portr_enabled: false,
        control_chat: false,
        opencode_bin: dir.join("opencode"),
        opencode_service_file: None,
        opencode_db: None,
        codex_home: dir.join("codex"),
        claude_home: dir.join("claude"),
        edit_responses: false,
        auto_forwarding: false,
        transcribe_command: vec![],
    }
}
fn agent() -> Agent {
    serde_json::from_value(json!({"pane_id":"w1:p1","terminal_id":"term1","tab_id":"w1:t1","agent":"opencode","agent_status":"idle","state_change_seq":1,"agent_session":{"agent":"opencode","source":"herdr:opencode","kind":"id","value":"ses_1"}})).unwrap()
}
fn inbound(sender: &str, id: &str) -> Value {
    json!({"marmot_agent_control":"marmot.agent-control.v2","type":"inbound_message","account_id_hex":"aa".repeat(32),"group_id_hex":"bb".repeat(16),"message":{"message_id_hex":id,"sender":{"account_id_hex":sender},"text":"test prompt","recorded_at":u64::MAX}})
}

fn accept_legacy_text(bridge: &mut Bridge, key: &str, id: &str, text: &str) {
    let mapping = bridge.state.mappings.get_mut(key).unwrap();
    if mapping.last_agent_status == "working" {
        mapping.pending_prompt = true;
    }
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: id.into(),
            group: mapping.group_id_hex.clone().unwrap(),
            text: text.into(),
            media: None,
        });
    mapping.seen_messages.push(id.into());
    bridge.save().unwrap();
}

async fn peer(
    path: &std::path::Path,
    marmot: bool,
    calls: Arc<Mutex<Vec<Value>>>,
) -> tokio::task::JoinHandle<()> {
    peer_for_agent(path, marmot, calls, agent()).await
}

async fn peer_for_agent(
    path: &std::path::Path,
    marmot: bool,
    calls: Arc<Mutex<Vec<Value>>>,
    observed: Agent,
) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        let mut profile_name = "HDR | historical".to_owned();
        let mut profile_description = String::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let req = read_line(&mut reader).await.unwrap();
            calls.lock().unwrap().push(req.clone());
            let mut response = if marmot {
                match req["type"].as_str().unwrap() {
                    "group_create_capabilities" => {
                        json!({"type":"group_create_capabilities","initial_admins":true})
                    }
                    "group_create" | "group_create_with_admins" => {
                        assert_eq!(req["type"], "group_create_with_admins");
                        assert_eq!(req["members"], json!([phone_npub()]));
                        assert_eq!(req["admins"], json!([phone_npub()]));
                        json!({"type":"group_created","group_id_hex":"bb".repeat(16)})
                    }
                    "send_final" | "send_media" => {
                        json!({"type":"final_sent","message_ids_hex":["cc".repeat(32)]})
                    }
                    "edit_message" => {
                        json!({"type":"final_sent","message_ids_hex":["dd".repeat(32)]})
                    }
                    "group_info" => {
                        json!({"type":"group_info","subject":profile_name,"description":profile_description})
                    }
                    "group_profile_update" => {
                        profile_name = req["name"].as_str().unwrap().into();
                        profile_description = req["description"].as_str().unwrap().into();
                        json!({"type":"group_profile_updated","message_ids_hex":["ee".repeat(32)]})
                    }
                    "download_media" => {
                        let path = req["media"]["test_path"].as_str().unwrap();
                        json!({"type":"media_downloaded","path":path,"media_type":req["media"]["media_type"],
                            "size_bytes":std::fs::metadata(path).unwrap().len()})
                    }
                    "timeline_message_get" => {
                        json!({"type":"timeline_message", "account_id_hex":"aa".repeat(32),
                        "group_id_hex":req["group_id_hex"],"message_id_hex":req["message_id_hex"],
                        "message":{"message_id_hex":req["message_id_hex"],"availability":"available","kind":9,
                            "sender":{"account_id_hex":HEX},"text":"", "attachments":[{"media_type":"audio/ogg","file_name":"voice.ogg"}]}})
                    }
                    "timeline_list" => {
                        json!({"type":"timeline_page","messages":[],"has_more_before":false})
                    }
                    _ => panic!("unexpected request"),
                }
            } else {
                match req["method"].as_str().unwrap() {
                    "session.snapshot" => {
                        json!({"result":{"snapshot":{"agents":[observed],"tabs":[{"tab_id":observed.tab_id,"label":"Review","number":1}]}}})
                    }
                    "agent.get" => json!({"result":{"agent":observed}}),
                    "agent.prompt" => json!({"result":{"type":"agent_prompted","agent":observed}}),
                    "agent.interrupt" => json!({"result":{"type":"ok"}}),
                    _ => panic!("unexpected method"),
                }
            };
            response["id"] = req["id"].clone();
            if marmot {
                response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            }
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            reader.get_mut().write_all(&bytes).await.unwrap();
        }
    })
}

fn export_script(path: &std::path::Path, text: &str, id: &str) {
    let data = json!({"messages":[{"info":{"id":id,"role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":text}]}]});
    std::fs::write(path, format!("#!/bin/sh\nprintf '%s' '{}'\n", data)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn control_reply_deadline_is_durable_without_delaying_native_phone_input() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), observed.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(observed.key(), observed.clone());
    bridge.state.mappings.insert(
        observed.key(),
        activity_mapping(observed.clone(), &"bb".repeat(16)),
    );
    let at = now() + 3;
    let mut help = inbound(HEX, "ordered-help");
    help["message"]["text"] = json!("/help");
    help["message"]["recorded_at"] = json!(at);
    bridge.inbound(&help).await.unwrap();
    let saved = BridgeState::load(&config.state_path).unwrap();
    assert_eq!(
        serde_json::to_value(&saved.mappings[&observed.key()]).unwrap()["manual_reply_after"],
        json!(at + 1)
    );
    // A future outgoing deadline must never become a wait before native agent input.
    let mut mapping = serde_json::to_value(&bridge.state.mappings[&observed.key()]).unwrap();
    mapping["manual_reply_after"] = json!(now() + 3600);
    bridge
        .state
        .mappings
        .insert(observed.key(), serde_json::from_value(mapping).unwrap());
    let mut text = inbound(HEX, "prompt-with-reply-pending");
    text["message"]["recorded_at"] = json!(at);
    tokio::time::timeout(std::time::Duration::from_secs(10), bridge.inbound(&text))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn delayed_control_replies_survive_restart_without_blocking_other_chats() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let observed = agent();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut mapping =
        serde_json::to_value(activity_mapping(observed.clone(), &"bb".repeat(16))).unwrap();
    mapping["manual_reply_after"] = json!(now() + 3600);
    bridge
        .state
        .mappings
        .insert(observed.key(), serde_json::from_value(mapping).unwrap());
    let group = "bb".repeat(16);
    let other = "cc".repeat(16);
    bridge
        .state
        .enqueue_manual(&group, "Delayed control reply", "ordered-control");
    bridge
        .state
        .enqueue_manual(&other, "Other chat reply", "other-control");
    bridge.save().unwrap();
    drop(bridge);
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0]["group_id_hex"], other);
    assert_eq!(bridge.state.outbox.len(), 1);
    assert_eq!(bridge.state.outbox[0].id, "ordered-control:0");
    let mut mapping = serde_json::to_value(&bridge.state.mappings[&observed.key()]).unwrap();
    mapping["manual_reply_after"] = json!(now());
    bridge
        .state
        .mappings
        .insert(observed.key(), serde_json::from_value(mapping).unwrap());
    bridge.save().unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(calls.lock().unwrap()[1]["group_id_hex"], group);
    assert!(bridge.state.outbox.is_empty());
    drop(bridge);
    Bridge::open(config).unwrap().flush().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn control_reply_deadlines_ignore_foreign_replayed_and_implausibly_future_input() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(root.path())).unwrap();
    let observed = agent();
    bridge.state.mappings.insert(
        observed.key(),
        activity_mapping(observed.clone(), &"bb".repeat(16)),
    );
    let mut help = inbound(&"22".repeat(32), "foreign-control");
    help["message"]["text"] = json!("/help");
    help["message"]["recorded_at"] = json!(now() + 3);
    bridge.inbound(&help).await.unwrap();
    assert_eq!(serde_json::to_value(&bridge.state.mappings[&observed.key()]).unwrap()["manual_reply_after"].as_u64().unwrap_or(0), 0);
    help = inbound(HEX, "future-control");
    help["message"]["text"] = json!("/help");
    bridge.inbound(&help).await.unwrap();
    assert_eq!(serde_json::to_value(&bridge.state.mappings[&observed.key()]).unwrap()["manual_reply_after"].as_u64().unwrap_or(0), 0);
    let at = now() + 3;
    help = inbound(HEX, "valid-control");
    help["message"]["text"] = json!("/help");
    help["message"]["recorded_at"] = json!(at);
    bridge.inbound(&help).await.unwrap();
    help["message"]["recorded_at"] = json!(now() + 5);
    bridge.inbound(&help).await.unwrap();
    assert_eq!(
        serde_json::to_value(&bridge.state.mappings[&observed.key()]).unwrap()["manual_reply_after"],
        json!(at + 1)
    );
}

#[tokio::test]
async fn phone_followups_are_submitted_while_working_without_interrupting_or_closing_the_response()
{
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.edit_responses = true;
    let mut observed = agent();
    observed.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), observed.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(observed.key(), observed.clone());
    let mut mapping = activity_mapping(observed.clone(), &"bb".repeat(16));
    mapping.pending_prompt = true;
    mapping.response_id = Some("current-response".into());
    bridge.state.mappings.insert(observed.key(), mapping);
    for (id, text) in [
        ("followup-one", "Please also check the tests"),
        ("followup-two", "And keep the current task running"),
    ] {
        let mut message = inbound(HEX, id);
        message["message"]["text"] = json!(text);
        bridge.inbound(&message).await.unwrap();
        bridge.inbound(&message).await.unwrap();
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.prompt")
            .count(),
        2
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["method"] == "agent.interrupt")
    );
    let mapping = &bridge.state.mappings[&observed.key()];
    assert!(mapping.prompt_queue.is_empty());
    assert_eq!(mapping.response_id.as_deref(), Some("current-response"));
    drop(bridge);
    let saved = Bridge::open(config).unwrap();
    assert!(
        saved.state.mappings[&observed.key()]
            .prompt_queue
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn cancel_clears_stale_questions_even_when_idle_and_persists_the_reset() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let observed = agent();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), observed.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut mapping = activity_mapping(observed.clone(), &"bb".repeat(16));
    mapping.decision = Some(serde_json::from_value(json!({
        "id":"stale-question", "form_id":"stale-form", "questions":[{"question":"Choose?", "options":[{"label":"Yes"}]}],
        "announced":true
    })).unwrap());
    mapping.permission =
        Some(serde_json::from_value(json!({"request":{"id":"stale-permission"}})).unwrap());
    mapping.decision_refresh_pending = true;
    bridge.state.mappings.insert(observed.key(), mapping);
    bridge.save().unwrap();
    let mut message = inbound(HEX, "reset-idle");
    message["message"]["text"] = json!("/cancel");
    bridge.inbound(&message).await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    let mapping = &bridge.state.mappings[&observed.key()];
    assert!(mapping.decision.is_none());
    assert!(mapping.permission.is_none());
    assert!(!mapping.decision_refresh_pending);
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] != "agent.interrupt")
    );
    bridge.herdr_available = true;
    bridge.agents.insert(observed.key(), observed.clone());
    let mut next = inbound(HEX, "after-reset");
    next["message"]["text"] = json!("Hello again");
    bridge.inbound(&next).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.prompt")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn cancel_interrupts_only_the_current_verified_session_once_without_clearing_accepted_work() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), observed.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(observed.key(), observed.clone());
    let mut mapping = activity_mapping(observed.clone(), &"bb".repeat(16));
    mapping.pending_prompt = true;
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "accepted-before-upgrade".into(),
            group: "bb".repeat(16),
            text: "Keep this delivery".into(),
            media: None,
        });
    bridge.state.mappings.insert(observed.key(), mapping);
    bridge.save().unwrap();
    let mut message = inbound(HEX, "cancel-current");
    message["message"]["text"] = json!("/cancel");
    bridge.inbound(&message).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.interrupt")
            .count(),
        1
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["method"] == "agent.prompt")
    );
    assert_eq!(bridge.state.mappings[&observed.key()].prompt_queue.len(), 1);
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.inbound(&message).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.interrupt")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn sync_modes_persist_ignore_automatic_changes_and_unmute_restores_automatic_mode() {
    use herdr_white_noise_bridge::forwarding;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    for (id, text, mode) in [
        ("force-on", "/sync on", "on"),
        ("force-off", "/sync off", "off"),
        ("automatic", "/sync auto", "auto"),
    ] {
        let mut message = inbound(HEX, id);
        message["message"]["text"] = json!(text);
        bridge.inbound(&message).await.unwrap();
        let saved: Value = serde_json::from_slice(
            &std::fs::read(config.state_path.with_extension("forwarding.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["mode"], mode);
        if mode == "on" {
            forwarding::set(&config.state_path, false).unwrap();
            assert!(forwarding::load(&config.state_path).unwrap().enabled);
        }
        if mode == "off" {
            forwarding::wake(&config.state_path, "group", "phone-message").unwrap();
            assert!(!forwarding::load(&config.state_path).unwrap().enabled);
        }
    }
    let mut force = inbound(HEX, "off-again");
    force["message"]["text"] = json!("/sync off");
    bridge.inbound(&force).await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut help = inbound(HEX, "mode-help");
    help["message"]["text"] = json!("/help");
    bridge.inbound(&help).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.contains("Current sync mode: Always off"))
    );
    let mut unmute = inbound(HEX, "clear-mode");
    unmute["message"]["text"] = json!("/unmute");
    // Restoring the control mode must be durable even while herdr is temporarily disconnected.
    let _ = bridge.inbound(&unmute).await;
    let saved: Value = serde_json::from_slice(
        &std::fs::read(config.state_path.with_extension("forwarding.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["mode"], "auto");
    assert!(saved["enabled"].as_bool().unwrap());
}

#[tokio::test]
async fn cancel_and_sync_mode_controls_reject_foreign_truncated_or_attached_input() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), observed.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(observed.key(), observed.clone());
    bridge.state.mappings.insert(
        observed.key(),
        activity_mapping(observed.clone(), &"bb".repeat(16)),
    );
    for command in ["/cancel", "/sync off"] {
        for field in [
            "foreign",
            "text_truncated",
            "attachments_truncated",
            "attachments",
            "media",
        ] {
            let mut msg = inbound(HEX, &format!("{command}-{field}"));
            msg["message"]["text"] = json!(command);
            match field {
                "foreign" => msg["message"]["sender"]["account_id_hex"] = json!("22".repeat(32)),
                "text_truncated" | "attachments_truncated" => msg["message"][field] = json!(true),
                _ => msg["message"][field] = json!([{"media_type":"image/png"}]),
            };
            bridge.inbound(&msg).await.unwrap();
        }
    }
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.interrupt")
    );
    assert!(
        herdr_white_noise_bridge::forwarding::load(&config.state_path)
            .unwrap()
            .enabled
    );
    server.abort();
}

#[tokio::test]
async fn a_busy_phone_submission_known_not_to_start_preserves_the_running_task_and_retry_receipt() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut working = agent();
    working.agent_status = "working".into();
    let listener = UnixListener::bind(&config.herdr_socket).unwrap();
    let path = config.herdr_socket.clone();
    let current = working.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = tokio::io::BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        assert_eq!(request["method"], "agent.get");
        std::fs::remove_file(path).unwrap();
        stream
            .get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"id":request["id"],"result":{"agent":current}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut bridge = Bridge::open(config).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(working.key(), working.clone());
    let mut mapping = activity_mapping(working.clone(), &"bb".repeat(16));
    mapping.pending_prompt = true;
    bridge.state.mappings.insert(working.key(), mapping);
    assert!(
        bridge
            .inbound(&inbound(HEX, "retry-fresh-input"))
            .await
            .is_err()
    );
    assert!(bridge.state.mappings[&working.key()].pending_prompt);
    assert!(
        !bridge.state.mappings[&working.key()]
            .seen_messages
            .contains(&"retry-fresh-input".to_string())
    );
    server.await.unwrap();
}

#[tokio::test]
async fn phone_text_in_an_ended_chat_reports_no_submission() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(root.path())).unwrap();
    bridge.herdr_available = true;
    let observed = agent();
    let mut mapping = activity_mapping(observed.clone(), &"bb".repeat(16));
    mapping.active = false;
    bridge.state.mappings.insert(observed.key(), mapping);
    bridge
        .inbound(&inbound(HEX, "ended-phone-text"))
        .await
        .unwrap();
    assert!(bridge.state.outbox.iter().any(|item| {
        item.text
            .contains("This session has ended. No prompt was sent.")
    }));
}

#[tokio::test]
async fn failed_phone_identity_refresh_keeps_the_receipt_retryable() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut working = agent();
    working.agent_status = "working".into();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(working.key(), working.clone());
    let mut mapping = activity_mapping(working.clone(), &"bb".repeat(16));
    mapping.pending_prompt = true;
    bridge.state.mappings.insert(working.key(), mapping);
    bridge.save().unwrap();
    let message = inbound(HEX, "retry-identity-refresh");
    assert!(bridge.inbound(&message).await.is_err());
    let saved = BridgeState::load(&config.state_path).unwrap();
    assert!(saved.mappings[&working.key()].pending_prompt);
    assert!(
        !saved.mappings[&working.key()]
            .seen_messages
            .contains(&"retry-identity-refresh".into())
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), working).await;
    bridge.herdr_available = true;
    bridge.inbound(&message).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn always_off_keeps_sync_muted_but_does_not_hold_a_phone_prompt() {
    for status in ["idle", "working"] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.auto_forwarding = true;
        let mut working = agent();
        working.agent_status = status.into();
        let calls = Arc::new(Mutex::new(vec![]));
        let server =
            peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.herdr_available = true;
        bridge.agents.insert(working.key(), working.clone());
        bridge.state.mappings.insert(
            working.key(),
            activity_mapping(working.clone(), &"bb".repeat(16)),
        );
        let mut off = inbound(HEX, "force-off");
        off["message"]["text"] = json!("/sync off");
        bridge.inbound(&off).await.unwrap();
        bridge
            .inbound(&inbound(HEX, "send-despite-off"))
            .await
            .unwrap();
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call["method"] == "agent.prompt")
                .count(),
            1
        );
        assert!(
            !herdr_white_noise_bridge::forwarding::load(&config.state_path)
                .unwrap()
                .enabled
        );
        assert!(!bridge.state.forwarding_enabled);
        server.abort();
    }
}

#[tokio::test]
async fn forwarding_cancellation_removes_only_unreferenced_saved_images() {
    use herdr_white_noise_bridge::{forwarding, outbound_images};
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.forwarding_enabled = true;
    for name in ["one.png", "two.png"] {
        std::fs::write(dir.path().join(name), b"\x89PNG\r\n\x1a\nfixture").unwrap();
    }
    let prepared = outbound_images::prepare(
        "![one](one.png) ![two](two.png)",
        dir.path(),
        &outbound_images::spool(&config.state_path),
    )
    .unwrap();
    let paths: Vec<_> = prepared
        .attachments
        .iter()
        .map(|a| a.path.clone())
        .collect();
    bridge.state.enqueue_images(
        "group",
        &prepared.text,
        prepared.attachments,
        vec!["source".into()],
    );
    let mut retained = bridge.state.outbox.last().unwrap().clone();
    retained.id = "manual-retained".into();
    retained.manual = true;
    retained.attachments.remove(0);
    bridge.state.outbox.push(retained);
    bridge.save().unwrap();
    forwarding::set(&config.state_path, false).unwrap();
    // The retained manual item cannot send without a connector, but must keep its image.
    assert!(bridge.flush().await.is_err());
    let saved = BridgeState::load(&config.state_path).unwrap();
    assert!(saved.outbox.iter().all(|item| item.manual));
    assert!(
        !std::path::Path::new(&paths[0]).exists(),
        "cancelled private snapshot must be removed after saving cancellation"
    );
    assert!(
        std::path::Path::new(&paths[1]).exists(),
        "retained references must keep their shared snapshot"
    );
    assert!(
        dir.path().join("one.png").exists(),
        "original image must not be deleted"
    );
}

#[tokio::test]
async fn failed_cancellation_save_does_not_remove_private_image_snapshot() {
    use herdr_white_noise_bridge::{forwarding, outbound_images};
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.forwarding_enabled = true;
    std::fs::write(dir.path().join("one.png"), b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let prepared = outbound_images::prepare(
        "![one](one.png)",
        dir.path(),
        &outbound_images::spool(&config.state_path),
    )
    .unwrap();
    let snapshot = prepared.attachments[0].path.clone();
    bridge.state.enqueue_images(
        "group",
        &prepared.text,
        prepared.attachments,
        vec!["source".into()],
    );
    bridge.save().unwrap();
    forwarding::set(&config.state_path, false).unwrap();
    // Make atomic state replacement fail, without touching any production state.
    let durable = std::fs::read(&config.state_path).unwrap();
    std::fs::remove_file(&config.state_path).unwrap();
    std::fs::create_dir(&config.state_path).unwrap();
    assert!(bridge.flush().await.is_err());
    assert!(
        std::path::Path::new(&snapshot).exists(),
        "a failed save must not delete a previously durable attachment"
    );
    std::fs::remove_dir(&config.state_path).unwrap();
    std::fs::write(&config.state_path, durable).unwrap();
    bridge.flush().await.unwrap();
    assert!(
        !std::path::Path::new(&snapshot).exists(),
        "storage recovery must retry durable cancellation and snapshot cleanup"
    );
    assert!(
        BridgeState::load(&config.state_path)
            .unwrap()
            .outbox
            .is_empty()
    );
}

#[tokio::test]
async fn explicit_reply_image_is_staged_and_delivered_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let image = dir.path().join("screen.png");
    let png = b"\x89PNG\r\n\x1a\nimage fixture";
    std::fs::write(&image, png).unwrap();
    export_script(
        &config.opencode_bin,
        "Here it is.\n\n![Screenshot](screen.png)\n\n[Download](bundle.tar.gz)",
        "image-final",
    );
    let mut observed = agent();
    observed.cwd = dir.path().to_string_lossy().into_owned();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        observed.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            agent_record: Some(observed.clone()),
            active: true,
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![observed.clone()]).await.unwrap();
    bridge.save().unwrap();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let lost_ack = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            if request["type"] == "send_media" {
                return request;
            }
            let response = json!({"id":request["id"],"marmot_agent_control":"marmot.agent-control.v2","type":"final_sent","message_ids_hex":["cc".repeat(32)]});
            reader
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    assert!(bridge.flush().await.is_err());
    let original = lost_ack.await.unwrap();
    assert_eq!(
        std::fs::read(original["attachments"][0]["path"].as_str().unwrap()).unwrap(),
        png
    );
    assert!(
        bridge.state.delivered_messages.is_empty(),
        "Source is not confirmed while its image is pending"
    );
    std::fs::remove_file(&config.wn_agent_socket).unwrap();
    // The original can disappear after staging without changing the retry bytes.
    std::fs::remove_file(image).unwrap();
    drop(bridge);
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut restarted = Bridge::open(config).unwrap();
    restarted.flush().await.unwrap();
    restarted.reconcile(vec![observed]).await.unwrap();
    restarted.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    let media: Vec<_> = calls.iter().filter(|r| r["type"] == "send_media").collect();
    assert_eq!(
        media.len(),
        1,
        "An embedded local image must be sent as media"
    );
    assert_eq!(media[0]["attachments"][0]["media_type"], "image/png");
    assert_eq!(media[0]["attachments"][0]["file_name"], "screen.png");
    assert!(!media[0]["idempotency_key"].as_str().unwrap().is_empty());
    assert_eq!(media[0]["idempotency_key"], original["idempotency_key"]);
    assert_eq!(media[0]["attachments"], original["attachments"]);
    assert!(
        !std::path::Path::new(original["attachments"][0]["path"].as_str().unwrap()).exists(),
        "Confirmed snapshots are removed only after the receipt is saved"
    );
    assert!(
        !calls.iter().any(|r| r["type"] == "send_final"),
        "Confirmed text must not be resent"
    );
    assert!(restarted.state.outbox.is_empty());
    assert!(!restarted.state.delivered_messages.is_empty());
    server.abort();
}

fn input_history(
    config: &BridgeConfig,
    session: &str,
    inputs: &[(&str, &str)],
    reply: Option<(&str, &str)>,
) {
    let mut messages: Vec<_> = inputs.iter().map(|(id, text)| json!({"info":{"id":id,"role":"user"},"parts":[{"type":"text","text":text}]})).collect();
    if let Some((id, text)) = reply {
        messages.push(json!({"info":{"id":id,"role":"assistant","parentID":inputs.last().unwrap().0,"finish":"stop","time":{"completed":1}},"parts":[{"type":"text","text":text}]}));
    }
    std::fs::write(
        config
            .opencode_bin
            .with_file_name(format!("{session}.json")),
        json!({"messages":messages}).to_string(),
    )
    .unwrap();
    std::fs::write(
        &config.opencode_bin,
        "#!/bin/sh\ncat \"$(dirname \"$0\")/$3.json\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn activity_mapping(observed: Agent, group: &str) -> Mapping {
    Mapping {
        group_id_hex: Some(group.into()),
        active: true,
        output_initialised: true,
        last_agent_status: "idle".into(),
        last_state_change_seq: 1,
        agent_record: Some(observed),
        ..Mapping::default()
    }
}

#[tokio::test]
async fn delivered_followup_reuses_current_status_without_a_new_working_bubble() {
    for editable in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.edit_responses = editable;
        input_history(&config, "ses_1", &[("prior", "Prior task")], None);
        let mut working = agent();
        working.agent_status = "working".into();
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let herdr =
            peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let key = working.key();
        let group = "bb".repeat(16);
        let mut mapping = activity_mapping(working.clone(), &group);
        mapping.pending_prompt = true;
        mapping.last_agent_status = "working".into();
        bridge.state.mappings.insert(key.clone(), mapping);
        if editable {
            bridge.state.begin_response(&key, &group);
            let response = bridge
                .state
                .outbox
                .last_mut()
                .unwrap()
                .response
                .as_mut()
                .unwrap();
            response.target = Some("old-progress".into());
            response.sent_text = Some("> working...".into());
        }
        bridge.reconcile(vec![working.clone()]).await.unwrap();
        bridge.flush().await.unwrap();
        calls.lock().unwrap().clear();
        let mut followup = inbound(HEX, "busy-followup");
        followup["message"]["text"] = json!("How is it going?");
        bridge.inbound(&followup).await.unwrap();
        bridge.inbound(&followup).await.unwrap();
        bridge.flush().await.unwrap();
        bridge.reconcile(vec![working.clone()]).await.unwrap();
        bridge.flush().await.unwrap();
        drop(bridge);
        let mut restarted = Bridge::open(config).unwrap();
        restarted.reconcile(vec![working]).await.unwrap();
        restarted.flush().await.unwrap();
        let calls = calls.lock().unwrap();
        let notices: Vec<_> = calls
            .iter()
            .filter(|r| r["type"] == "send_final")
            .map(|r| r["text"].as_str().unwrap())
            .collect();
        assert!(
            notices.is_empty(),
            "Follow-ups must not create another status bubble: {notices:?}"
        );
        assert!(restarted.state.mappings[&key].prompt_queue.is_empty());
        assert_eq!(
            calls
                .iter()
                .filter(|r| r["method"] == "agent.prompt")
                .count(),
            1
        );
        mdk.abort();
        herdr.abort();
    }
}

#[tokio::test]
async fn resuming_a_decision_edits_the_existing_status_instead_of_posting_another() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.edit_responses = true;
    input_history(&config, "ses_1", &[("prior", "Prior task")], None);
    let mut working = agent();
    working.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = working.key();
    let group = "bb".repeat(16);
    let mut mapping = activity_mapping(working.clone(), &group);
    mapping.last_agent_status = "blocked".into();
    mapping.pending_prompt = true;
    bridge.state.mappings.insert(key.clone(), mapping);
    bridge.state.begin_response(&key, &group);
    let item = bridge.state.outbox.last_mut().unwrap();
    item.text = "> blocked Waiting for your decision.".into();
    item.response.as_mut().unwrap().target = Some("old-blocked-bubble".into());
    item.response.as_mut().unwrap().sent_text = Some(item.text.clone());
    bridge.reconcile(vec![working.clone()]).await.unwrap();
    bridge.flush().await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![working]).await.unwrap();
    bridge.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|r| r["type"] == "send_final" && r["text"] == "> working...")
            .count(),
        0,
        "Resume must reuse the existing editable status"
    );
    assert!(
        calls
            .iter()
            .any(|r| r["type"] == "edit_message"
                && r["target_message_id_hex"] == "old-blocked-bubble")
    );
    mdk.abort();
}

#[tokio::test]
async fn failed_working_refresh_cannot_hold_back_a_final_answer() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut working = agent();
    working.agent_status = "working".into();
    let group = "bb".repeat(16);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(working.key(), activity_mapping(working.clone(), &group));
    bridge.agents.insert(working.key(), working);
    bridge.state.enqueue_working_notice(&group);
    bridge.state.enqueue(&group, "The final answer");
    bridge.save().unwrap();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let requests = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            requests.lock().unwrap().push(request.clone());
            let mut response = if request["text"] == "> working..." {
                json!({"type":"error","code":"temporary_failure"})
            } else {
                json!({"type":"final_sent","message_ids_hex":["cc".repeat(32)]})
            };
            response["id"] = request["id"].clone();
            response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            reader
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let result = bridge.flush().await;
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["text"] == "The final answer"),
        "A failed optional status must not block the answer"
    );
    assert!(
        result.is_ok(),
        "An optional status failure must not fail durable delivery"
    );
    assert_eq!(calls.lock().unwrap()[0]["text"], "The final answer");
    assert!(bridge.state.outbox.is_empty());
    server.abort();
}

#[tokio::test]
async fn legacy_working_refresh_is_dropped_instead_of_adding_another_bubble() {
    for status in ["working", "blocked", "done", "ended"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let mut observed = agent();
        observed.agent_status = status.into();
        let group = "bb".repeat(16);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge
            .state
            .mappings
            .insert(observed.key(), activity_mapping(observed.clone(), &group));
        bridge.state.enqueue_working_notice(&group);
        if status != "ended" {
            bridge.agents.insert(observed.key(), observed);
        }
        bridge.save().unwrap();
        let calls = Arc::new(Mutex::new(vec![]));
        let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
        bridge.flush().await.unwrap();
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request["text"] == "> working..."),
            "Do not deliver an obsolete working status for {status}"
        );
        assert!(bridge.state.outbox.is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn delivered_or_rejected_followup_does_not_claim_working_for_a_blocked_or_settled_agent() {
    for status in ["blocked", "done"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        input_history(&config, "ses_1", &[], None);
        let mut observed = agent();
        observed.agent_status = status.into();
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = peer_for_agent(&config.herdr_socket, false, calls, observed.clone()).await;
        let mut bridge = Bridge::open(config).unwrap();
        let mut mapping = activity_mapping(observed.clone(), &"bb".repeat(16));
        mapping.last_agent_status = status.into();
        mapping.pending_prompt = true;
        bridge.state.mappings.insert(observed.key(), mapping);
        bridge.reconcile(vec![observed]).await.unwrap();
        let mut followup = inbound(HEX, "followup");
        followup["message"]["text"] = json!("How is it going?");
        bridge.inbound(&followup).await.unwrap();
        assert!(
            bridge
                .state
                .mappings
                .values()
                .all(|mapping| mapping.prompt_queue.is_empty())
        );
        assert!(
            !bridge
                .state
                .outbox
                .iter()
                .any(|item| item.text.starts_with("> working"))
        );
        herdr.abort();
    }
}

fn inbound_image(path: &std::path::Path, id: &str, caption: &str) -> Value {
    let mut event = inbound(HEX, id);
    event["message"]["text"] = json!(caption);
    event["message"]["media"] = json!([{"media_type":"image/png","test_path":path,"file_name":"screen.png",
        "ciphertext_sha256":"aa".repeat(32),"plaintext_sha256":"bb".repeat(32),"nonce_hex":"00".repeat(12),
        "version":"encrypted-media-v1","source_epoch":1,"locators":[{"kind":"blossom-v1","value":"https://example.com/fixture"}]}]);
    event
}

#[tokio::test]
async fn downloaded_images_reach_a_working_agent_without_waiting_for_a_final() {
    for sync_off in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.edit_responses = true;
        input_history(&config, "ses_1", &[("prior", "Current task")], None);
        let image = root.path().join("screen.png");
        std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let mut working = agent();
        working.agent_status = "working".into();
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let herdr =
            peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let key = working.key();
        let group = "bb".repeat(16);
        let mut mapping = activity_mapping(working.clone(), &group);
        mapping.pending_prompt = true;
        mapping.last_agent_status = "working".into();
        mapping.queue_flight = Some(herdr_white_noise_bridge::state::QueueFlight {
            before_final: Some("current-task-final".into()),
            confirmed: false,
        });
        bridge.state.mappings.insert(key.clone(), mapping);
        bridge.state.begin_response(&key, &group);
        let flight = bridge.state.mappings[&key].queue_flight.clone();
        bridge.reconcile(vec![working.clone()]).await.unwrap();
        if sync_off {
            herdr_white_noise_bridge::forwarding::set_mode(
                &config.state_path,
                herdr_white_noise_bridge::forwarding::Mode::Off,
            )
            .unwrap();
        }
        accept_legacy_text(
            &mut bridge,
            &key,
            "legacy",
            "Keep this older accepted delivery",
        );
        for (id, caption) in [("image-one", "First image"), ("image-two", "Second image")] {
            let event = inbound_image(&image, id, caption);
            bridge.inbound(&event).await.unwrap();
            bridge.inbound(&event).await.unwrap();
        }
        let response = bridge.state.mappings[&key].response_id.clone();
        bridge.reconcile(vec![working.clone()]).await.unwrap();
        assert!(
            !bridge.state.media_pending.is_empty(),
            "Image download must start while working, even behind a legacy delivery"
        );
        for _ in 0..1000 {
            bridge.process_media().await.unwrap();
            bridge.reconcile(vec![working.clone()]).await.unwrap();
            if calls
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "agent.prompt")
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let requests = calls.lock().unwrap();
        let prompts: Vec<_> = requests
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .collect();
        assert_eq!(prompts.len(), 2);
        assert!(
            prompts[0]["params"]["text"]
                .as_str()
                .unwrap()
                .starts_with("First image\n\nAttached image: ")
        );
        assert!(
            prompts[1]["params"]["text"]
                .as_str()
                .unwrap()
                .starts_with("Second image\n\nAttached image: ")
        );
        assert!(!requests.iter().any(|r| r["method"] == "agent.interrupt"));
        assert_eq!(bridge.state.mappings[&key].queue_flight, flight);
        assert_eq!(bridge.state.mappings[&key].response_id, response);
        assert_eq!(bridge.state.mappings[&key].prompt_queue.len(), 1);
        assert_eq!(bridge.state.mappings[&key].prompt_queue[0].id, "legacy");
        assert!(!bridge.state.outbox.iter().any(|m| m.working_notice));
        assert!(
            !bridge
                .state
                .outbox
                .iter()
                .any(|m| m.text.starts_with("Queued prompt"))
        );
        mdk.abort();
        herdr.abort();
    }
}

#[tokio::test]
async fn voice_is_prepared_and_delivered_while_the_agent_is_working() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let command = root.path().join("transcribe");
    std::fs::write(&command, "#!/bin/sh\nprintf 'Also check the tests.\\n'\n").unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.transcribe_command = vec![command.to_string_lossy().into_owned()];
    let audio = root.path().join("voice.ogg");
    std::fs::write(&audio, b"audio fixture").unwrap();
    let mut working = agent();
    working.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    let mut mapping = activity_mapping(working.clone(), &"bb".repeat(16));
    mapping.pending_prompt = true;
    bridge.state.mappings.insert(working.key(), mapping);
    bridge.herdr_available = true;
    bridge.agents.insert(working.key(), working);
    let mut event = inbound_image(&audio, "busy-voice", "Follow-up");
    event["message"]["media"][0]["media_type"] = json!("audio/ogg");
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        bridge.state.media_pending.len(),
        1,
        "Transcription must start while working"
    );
    for _ in 0..1000 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    bridge.inbound(&event).await.unwrap();
    let requests = calls.lock().unwrap();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(
        prompts[0]["params"]["text"],
        "Follow-up\n\nAlso check the tests."
    );
    assert!(!requests.iter().any(|r| r["method"] == "agent.interrupt"));
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn zip_files_survive_preparation_restart_and_reach_a_working_agent_once_even_with_sync_off() {
    for sync_off in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let file = root.path().join("logs.zip");
        std::fs::write(&file, b"PK\x03\x04private log fixture").unwrap();
        if sync_off {
            herdr_white_noise_bridge::forwarding::set_mode(
                &config.state_path,
                herdr_white_noise_bridge::forwarding::Mode::Off,
            )
            .unwrap();
        }
        let mut working = agent();
        working.agent_status = "working".into();
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let herdr =
            peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let key = working.key();
        let mut mapping = activity_mapping(working.clone(), &"bb".repeat(16));
        mapping.last_agent_status = "working".into();
        mapping.pending_prompt = true;
        mapping.queue_flight = Some(herdr_white_noise_bridge::state::QueueFlight {
            before_final: Some("older-task".into()),
            confirmed: false,
        });
        let flight = mapping.queue_flight.clone();
        bridge.state.mappings.insert(key.clone(), mapping);
        bridge.agents.insert(key.clone(), working.clone());
        bridge.herdr_available = true;
        let mut event = inbound_image(&file, "zip-attachment", "Inspect these logs.");
        event["message"]["media"][0]["media_type"] = json!("application/zip");
        event["message"]["media"][0]["file_name"] = json!("white-noise-diagnostic-logs.zip");
        bridge.inbound(&event).await.unwrap();
        bridge.inbound(&event).await.unwrap();
        assert_eq!(bridge.state.media_pending.len(), 1);
        bridge.herdr_available = false;
        for _ in 0..1000 {
            let _ = bridge.process_media().await;
            if bridge
                .state
                .media_pending
                .first()
                .is_some_and(|e| e["_codr_prepared_text"].is_string())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let prompt = bridge.state.media_pending[0]["_codr_prepared_text"]
            .as_str()
            .expect("ZIP preparation must finish without agent idle")
            .to_owned();
        assert!(prompt.starts_with("Inspect these logs.\n\nAttached file: "));
        let path: String = serde_json::from_str(
            prompt
                .split_once("Attached file: ")
                .unwrap()
                .1
                .split_once('\n')
                .unwrap()
                .0,
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&file).unwrap();
        bridge.save().unwrap();
        drop(bridge);
        let mut bridge = Bridge::open(config).unwrap();
        bridge.agents.insert(key.clone(), working);
        bridge.herdr_available = true;
        // Advance the persisted retry deadline after herdr recovery.
        bridge.state.media_pending[0]["_codr_retry_at"] = json!(0);
        bridge.process_media().await.unwrap();
        bridge.inbound(&event).await.unwrap();
        bridge.process_media().await.unwrap();
        assert!(bridge.state.media_pending.is_empty());
        assert_eq!(bridge.state.mappings[&key].queue_flight, flight);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            !bridge
                .state
                .outbox
                .iter()
                .any(|item| item.text.contains("transcrib") || item.text == "> downloading")
        );
        let requests = calls.lock().unwrap();
        let prompts: Vec<_> = requests
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["params"]["text"], prompt);
        assert!(!requests.iter().any(|r| r["method"] == "agent.interrupt"));
        mdk.abort();
        herdr.abort();
    }
}

#[tokio::test]
async fn ready_media_is_not_held_by_an_unrelated_download_in_progress() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let image = root.path().join("slow.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let mut working = agent();
    working.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let (started, received) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let download_path = image.clone();
    let mdk = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = tokio::io::BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        assert_eq!(request["type"], "download_media");
        started.send(()).unwrap();
        wait.await.unwrap();
        let response = json!({"marmot_agent_control":"marmot.agent-control.v2","id":request["id"],"type":"media_downloaded","path":download_path,"media_type":"image/png","size_bytes":std::fs::metadata(download_path).unwrap().len()});
        stream
            .get_mut()
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
    });
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        working.key(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.herdr_available = true;
    bridge.agents.insert(working.key(), working);
    bridge
        .state
        .media_pending
        .push(inbound_image(&image, "slow", "Slow image"));
    bridge.process_media().await.unwrap();
    received.await.unwrap();
    let mut ready = inbound(HEX, "ready");
    ready["_codr_prepared_text"] = json!("Ready transcript");
    bridge.state.media_pending.push(ready);
    bridge.process_media().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "agent.prompt" && r["params"]["text"] == "Ready transcript"),
        "Prepared input must not wait for another download"
    );
    assert_eq!(bridge.state.media_pending.len(), 1);
    assert_eq!(
        bridge.state.media_pending[0]["message"]["message_id_hex"],
        "slow"
    );
    release.send(()).unwrap();
    mdk.await.unwrap();
    for _ in 0..1000 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(bridge.state.media_pending.is_empty());
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .count(),
        2
    );
    herdr.abort();
}

#[tokio::test]
async fn prepared_media_after_restart_cannot_cross_a_changed_native_session() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[("prior", "Current task")], None);
    let mut working = agent();
    working.agent_status = "working".into();
    let mut changed = working.clone();
    changed.agent_session.as_mut().unwrap().value = "ses_changed".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), changed).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = working.key();
    let group = "bb".repeat(16);
    let mut mapping = activity_mapping(working.clone(), &group);
    mapping.pending_prompt = true;
    let mut message =
        inbound_image(&root.path().join("saved.png"), "prepared-image", "Caption")["message"]
            .clone();
    message["_codr_prepared_text"] = json!("Caption\n\nAttached image: \"saved.png\"");
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "prepared-image".into(),
            group,
            text: "Caption".into(),
            media: Some(message),
        });
    mapping.seen_messages.push("prepared-image".into());
    bridge.state.mappings.insert(key.clone(), mapping);
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![working]).await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "agent.prompt")
    );
    assert_eq!(bridge.state.mappings[&key].prompt_queue.len(), 1);
    assert_eq!(
        bridge.state.mappings[&key].prompt_queue[0].id,
        "prepared-image"
    );
    herdr.abort();
}

#[tokio::test]
async fn prepared_media_is_retained_when_the_pre_submission_state_save_fails() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut working = agent();
    working.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = working.key();
    bridge.state.mappings.insert(
        key.clone(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.agents.insert(key.clone(), working);
    bridge.herdr_available = true;
    let mut event = inbound(HEX, "prepared-save-failure");
    event["_codr_prepared_text"] = json!("Prepared transcript");
    bridge.state.media_pending.push(event);
    std::fs::remove_file(&config.state_path).unwrap();
    std::fs::create_dir(&config.state_path).unwrap();
    assert!(bridge.process_media().await.is_err());
    assert!(
        !bridge.state.mappings[&key]
            .seen_messages
            .iter()
            .any(|id| id == "prepared-save-failure")
    );
    assert_eq!(bridge.state.media_pending.len(), 1);
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "agent.prompt")
    );
    herdr.abort();
}

#[tokio::test]
async fn prepared_transcript_that_looks_like_status_is_still_agent_input() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut working = agent();
    working.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        working.key(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.agents.insert(working.key(), working);
    bridge.herdr_available = true;
    let mut event = inbound(HEX, "spoken-status");
    event["_codr_prepared_text"] = json!("/status");
    bridge.state.media_pending.push(event);
    bridge.process_media().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "agent.prompt" && r["params"]["text"] == "/status")
    );
    assert!(bridge.state.media_pending.is_empty());
    herdr.abort();
}

#[tokio::test]
async fn known_unsent_ready_image_returns_to_its_slot_without_resetting_an_older_flight() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[("prior", "Current task")], None);
    let mut working = agent();
    working.agent_status = "working".into();
    let listener = UnixListener::bind(&config.herdr_socket).unwrap();
    let path = config.herdr_socket.clone();
    let observed = working.clone();
    let preflight = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = tokio::io::BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        assert_eq!(request["method"], "agent.get");
        std::fs::remove_file(path).unwrap();
        let response = json!({"id":request["id"],"result":{"agent":observed}});
        stream
            .get_mut()
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
    });
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = working.key();
    let group = "bb".repeat(16);
    let mut mapping = activity_mapping(working.clone(), &group);
    mapping.pending_prompt = true;
    mapping.queue_flight = Some(herdr_white_noise_bridge::state::QueueFlight {
        before_final: Some("older-final".into()),
        confirmed: false,
    });
    let flight = mapping.queue_flight.clone();
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "legacy".into(),
            group: group.clone(),
            text: "Keep older delivery".into(),
            media: None,
        });
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "ready-image".into(),
            group,
            text: "Ready image".into(),
            media: Some(json!({"_codr_prepared_text":"Ready image with cached private path"})),
        });
    mapping
        .seen_messages
        .extend(["legacy".into(), "ready-image".into()]);
    bridge.state.mappings.insert(key.clone(), mapping);
    bridge.save().unwrap();
    bridge.reconcile(vec![working.clone()]).await.unwrap();
    preflight.await.unwrap();
    assert_eq!(bridge.state.mappings[&key].queue_flight, flight);
    assert!(bridge.state.mappings[&key].pending_prompt);
    assert_eq!(
        bridge.state.mappings[&key]
            .prompt_queue
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["legacy", "ready-image"]
    );
    drop(bridge);
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![working.clone()]).await.unwrap();
    bridge.reconcile(vec![working]).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .count(),
        1
    );
    assert_eq!(bridge.state.mappings[&key].queue_flight, flight);
    assert_eq!(bridge.state.mappings[&key].prompt_queue.len(), 1);
    assert_eq!(bridge.state.mappings[&key].prompt_queue[0].id, "legacy");
    herdr.abort();
}

#[tokio::test]
async fn image_prompt_queue_does_not_block_a_spoken_decision_answer() {
    let root = tempfile::tempdir().unwrap();
    let mut config = editable_config(root.path());
    decision_export(&config.opencode_bin, "question-1", "Which next step?");
    let command = root.path().join("transcribe");
    std::fs::write(&command, "#!/bin/sh\nprintf '1\\n'\n").unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.transcribe_command = vec![command.to_string_lossy().into_owned()];
    let recording = root.path().join("voice.ogg");
    std::fs::write(&recording, "audio fixture").unwrap();
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let visible = Arc::new(Mutex::new(decision_terminal("Which next step?")));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(&config.herdr_socket, observed, visible, calls.clone()).await;
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![blocked]).await.unwrap();
    let image = inbound_image(
        &root.path().join("later.png"),
        "waiting-image",
        "Later screenshot",
    );
    bridge.inbound(&image).await.unwrap();
    bridge.inbound(&inbound(HEX, "waiting-text")).await.unwrap();
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    let mut voice = inbound_image(&recording, "spoken-decision", "");
    voice["message"]["media"][0]["media_type"] = json!("audio/ogg");
    voice["message"]["media"][0]["file_name"] = json!("voice.ogg");
    bridge.inbound(&voice).await.unwrap();
    assert_eq!(
        bridge.state.media_pending.len(),
        1,
        "Voice decisions bypass waiting images and text"
    );
    for _ in 0..100 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    bridge.inbound(&voice).await.unwrap();
    let calls = calls.lock().unwrap();
    let decisions: Vec<_> = calls
        .iter()
        .filter(|call| call["method"] == "agent.send_keys")
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["params"]["keys"], json!(["1"]));
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    assert!(!calls.iter().any(|call| call["method"] == "agent.prompt"));
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn image_prompt_queue_preserves_caption_fifo_and_prepared_files_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[("prior-input", "Prior task")], None);
    let image = root.path().join("screen.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let prompts = || {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.prompt")
            .map(|c| c["params"]["text"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let mut working = agent();
    working.agent_status = "working".into();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        working.key(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![working]).await.unwrap();
    let event = inbound_image(&image, "queued-image", "Review this screenshot");
    bridge.inbound(&event).await.unwrap();
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&agent().key()].prompt_queue.len(),
        1,
        "A busy image is accepted once into the FIFO"
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.starts_with("Preparing image."))
    );
    accept_legacy_text(&mut bridge, &agent().key(), "text-after-image", "Next task");
    assert!(prompts().is_empty());
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    input_history(
        &config,
        "ses_1",
        &[("prior-input", "Prior task")],
        Some(("prior-final", "Prior answer")),
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.herdr_available = false;
    for _ in 0..100 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(bridge.state.media_pending.is_empty());
    assert!(
        prompts().is_empty(),
        "Prepared images remain durable while herdr is unavailable"
    );
    std::fs::remove_file(image).unwrap();
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(
        prompts().len(),
        1,
        "Prepared owner images are delivered even while outgoing sync is off"
    );
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    let sent = prompts();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].starts_with("Review this screenshot\n\nAttached image: "));
    let saved_path: String =
        serde_json::from_str(sent[0].split("Attached image: ").nth(1).unwrap()).unwrap();
    assert!(std::path::Path::new(&saved_path).is_file());
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(prompts().len(), 1);
    input_history(
        &config,
        "ses_1",
        &[("image-input", &sent[0])],
        Some(("image-final", "Image reviewed")),
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(prompts(), [sent[0].clone(), "Next task".into()]);
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn retired_clear_queue_during_preparation_keeps_the_accepted_image() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let image = root.path().join("screen.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![agent()]).await.unwrap();
    let image_event = inbound_image(&image, "cancelled-image", "Screenshot");
    bridge.inbound(&image_event).await.unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.process_media().await.unwrap();
    let mut clear = inbound(HEX, "clear-images");
    clear["message"]["text"] = json!("/clear-queue");
    bridge.inbound(&clear).await.unwrap();
    for _ in 0..100 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(bridge.state.media_pending.is_empty());
    bridge.inbound(&image_event).await.unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .prompt_queue
            .is_empty()
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["method"] == "agent.prompt")
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.starts_with("/clear-queue has been retired"))
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn image_prompt_queue_shares_limits_and_rejects_foreign_or_truncated_input() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let image = root.path().join("screen.png");
    let mut working = agent();
    working.agent_status = "working".into();
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        working.key(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![working]).await.unwrap();
    let mut foreign = inbound_image(&image, "foreign", "Screenshot");
    foreign["message"]["sender"]["account_id_hex"] = json!("99".repeat(32));
    bridge.inbound(&foreign).await.unwrap();
    for field in ["text_truncated", "attachments_truncated"] {
        let mut truncated = inbound_image(&image, field, "Screenshot");
        truncated["message"][field] = json!(true);
        bridge.inbound(&truncated).await.unwrap();
    }
    assert!(
        bridge.state.mappings[&agent().key()]
            .prompt_queue
            .is_empty()
    );
    for i in 0..17 {
        let mut event = inbound_image(&image, &format!("image-{i}"), "Screenshot");
        event["message"]["recorded_at"] = json!(u64::MAX);
        bridge.inbound(&event).await.unwrap();
    }
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 16);
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.starts_with("Media preparation limit reached"))
    );
    assert!(
        bridge.state.media_pending.is_empty(),
        "Acceptance alone does not start synchronous downloads; the worker schedules preparation"
    );
}

#[tokio::test]
async fn image_prompt_queue_hydrates_refs_and_keeps_command_like_captions_as_input() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let image = root.path().join("screen.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![agent()]).await.unwrap();
    let complete = inbound_image(&image, "summary-image", "/allow example");
    let mut summary = complete.clone();
    summary["message"]["media"] = json!([{"media_type":"image/png","file_name":"screen.png"}]);
    bridge.inbound(&summary).await.unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.process_media().await.unwrap();
    bridge.inbound(&complete).await.unwrap();
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    for _ in 0..100 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    bridge.reconcile(vec![agent()]).await.unwrap();
    let calls = calls.lock().unwrap();
    let prompts: Vec<_> = calls
        .iter()
        .filter(|c| c["method"] == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0]["params"]["text"]
            .as_str()
            .unwrap()
            .starts_with("/allow example\n\nAttached image: ")
    );
    assert!(!calls.iter().any(|c| c["method"] == "agent.send_keys"));
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn image_prompt_queue_failure_releases_the_head_without_replaying_it() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let image = root.path().join("too-big.png");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(25 * 1024 * 1024 + 1)
        .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![agent()]).await.unwrap();
    let image_event = inbound_image(&image, "failed-image", "Too big");
    bridge.inbound(&image_event).await.unwrap();
    accept_legacy_text(
        &mut bridge,
        &agent().key(),
        "after-failed-image",
        "Next task",
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    for _ in 0..100 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.starts_with("Attachment preparation failed"))
    );
    bridge.inbound(&image_event).await.unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    let calls = calls.lock().unwrap();
    let prompts: Vec<_> = calls
        .iter()
        .filter(|c| c["method"] == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["params"]["text"], "Next task");
    mdk.abort();
    herdr.abort();
}

/// Help is a private, replay-safe command response, not a prompt or a global wake.
#[tokio::test]
async fn shortcut_help_replies_in_its_own_chat_while_muted_without_herdr() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let mut event = inbound(HEX, "help-command");
    bridge
        .state
        .enqueue(&"bb".repeat(16), "Automatic output must stay muted");
    event["message"]["text"] = json!(" /HeLp \n");
    bridge.inbound(&event).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(!bridge.state.forwarding_enabled);
    assert!(
        !herdr_white_noise_bridge::forwarding::load(&config.state_path)
            .unwrap()
            .enabled
    );
    let sent: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c["type"] == "send_final")
        .cloned()
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["group_id_hex"], "bb".repeat(16));
    let text = sent[0]["text"].as_str().unwrap();
    for shortcut in ["/help", "/recent", "/status", "/unmute"] {
        assert!(text.contains(shortcut));
    }
    drop(bridge);
    let mut reopened = Bridge::open(config).unwrap();
    reopened.inbound(&event).await.unwrap();
    reopened.flush().await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "send_final")
            .count(),
        1
    );
    mdk.abort();
}

/// A manual catch-up includes only recent linked conversations and does not consume finals.
#[tokio::test]
async fn shortcut_sync_sends_recent_excerpts_to_their_own_chats_without_unmuting() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    for (session, group, age) in [
        ("ses_1", "bb".repeat(16), 20),
        ("ses_other", "dd".repeat(16), 60),
        ("ses_old", "ee".repeat(16), 7200),
        ("ses_future", "ab".repeat(16), 0),
    ] {
        let mut observed = agent();
        observed.agent_session.as_mut().unwrap().value = session.into();
        let mut mapping = activity_mapping(observed.clone(), &group);
        mapping.final_ids.push("old-checkpoint".into());
        mapping.pending_prompt = true;
        mapping.active = false; // Recently ended conversations are still eligible.
        bridge.state.mappings.insert(observed.key(), mapping);
        input_history(
            &config,
            session,
            &[("user", "Local prompt")],
            Some(("last", session)),
        );
        let path = config
            .opencode_bin
            .with_file_name(format!("{session}.json"));
        let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data["messages"][0]["info"]["time"] =
            json!({"created":(if age == 0 { now()+3600 } else { now()-age })*1000});
        std::fs::write(path, data.to_string()).unwrap();
    }
    let mut missing = agent();
    missing.agent_session.as_mut().unwrap().value = "ses_missing".into();
    bridge
        .state
        .mappings
        .insert(missing.key(), activity_mapping(missing, &"ff".repeat(16)));
    let before = bridge.state.mappings.clone();
    bridge.save().unwrap();
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let mut event = inbound(HEX, "sync-command");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(!bridge.state.forwarding_enabled);
    assert!(
        !herdr_white_noise_bridge::forwarding::load(&config.state_path)
            .unwrap()
            .enabled
    );
    for (key, mapping) in &before {
        assert_eq!(bridge.state.mappings[key].final_ids, mapping.final_ids);
        assert_eq!(
            bridge.state.mappings[key].pending_prompt,
            mapping.pending_prompt
        );
    }
    let sent: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c["type"] == "send_final")
        .cloned()
        .collect();
    assert!(
        sent.iter().any(|c| c["group_id_hex"] == "bb".repeat(16)
            && c["text"].as_str().unwrap().contains("ses_1"))
    );
    assert!(sent.iter().any(|c| c["group_id_hex"] == "dd".repeat(16)
        && c["text"].as_str().unwrap().contains("ses_other")));
    assert!(!sent.iter().any(|c| c["group_id_hex"] == "ee".repeat(16)));
    assert!(
        !sent
            .iter()
            .any(|c| c["group_id_hex"] == "ab".repeat(16) || c["group_id_hex"] == "ff".repeat(16))
    );
    assert!(
        sent.iter()
            .any(|c| { c["text"].as_str().unwrap().contains("1 unavailable") })
    );
    assert!(sent.iter().any(|c| {
        c["text"].as_str().unwrap().contains("**You:**\n_")
            && c["text"].as_str().unwrap().contains("\nLocal prompt")
    }));
    let count = sent.len();
    let summary = sent
        .iter()
        .find(|call| call["text"].as_str().unwrap().starts_with("Recent:"))
        .unwrap()["text"]
        .as_str()
        .unwrap();
    assert!(
        summary.len() <= 100,
        "Sync status must fit a short phone notification"
    );
    assert!(!summary.contains('\n'));
    assert!(summary.starts_with("Recent: 2 chats queued; 1 unavailable"));
    drop(bridge);
    let mut reopened = Bridge::open(config).unwrap();
    reopened.inbound(&event).await.unwrap();
    reopened.flush().await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "send_final")
            .count(),
        count
    );
    mdk.abort();
}

/// Catch-up includes conversational context, not only the most recent answer.
#[tokio::test]
async fn shortcut_sync_sends_last_three_conversation_messages_with_speaker_labels() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let data = json!({"messages":[
        {"info":{"id":"old-user","role":"user"},"parts":[{"type":"text","text":"Older prompt"}]},
        {"info":{"id":"first","role":"assistant","finish":"stop","time":{"completed":1}},"parts":[{"type":"text","text":"Earlier answer"}]},
        {"info":{"id":"question","role":"user"},"parts":[{"type":"text","text":"Follow-up question"},{"type":"text","text":"hidden instruction","synthetic":true}]},
        {"info":{"id":"tool","role":"assistant","finish":"tool-calls","time":{"completed":1}},"parts":[{"type":"text","text":"Tool commentary"}]},
        {"info":{"id":"last","role":"assistant","finish":"stop","time":{"completed":now()*1000}},"parts":[{"type":"text","text":"Latest answer"}]},
        {"info":{"id":"streaming","role":"assistant","finish":"stop"},"parts":[{"type":"text","text":"Unfinished reply"}]}
    ]});
    std::fs::write(
        config.opencode_bin.with_file_name("ses_1.json"),
        data.to_string(),
    )
    .unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let mut event = inbound(HEX, "three-message-sync");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    let last_date = chrono::DateTime::from_timestamp(
        data["messages"][4]["info"]["time"]["completed"]
            .as_u64()
            .unwrap() as i64
            / 1000,
        0,
    )
    .unwrap()
    .format("%Y-%m-%d %H:%M:%S UTC")
    .to_string();
    assert!(bridge.state.outbox.iter().any(|item| item.manual && item.text == format!("**Agent:**\n_1970-01-01 00:00:00 UTC_\nEarlier answer\n\n---\n\n**You:**\n_Date unknown_\nFollow-up question\n\n---\n\n**Agent:**\n_{last_date}_\nLatest answer")));
    assert!(!bridge.state.forwarding_enabled);
    let batch = bridge.state.outbox.clone();
    drop(bridge);
    let mut reopened = Bridge::open(config).unwrap();
    reopened.inbound(&event).await.unwrap();
    assert_eq!(reopened.state.outbox, batch);
}

#[tokio::test]
async fn shortcut_help_is_a_short_command_list() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(root.path())).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    let mut event = inbound(HEX, "current-help");
    event["message"]["text"] = json!("/help");
    bridge.inbound(&event).await.unwrap();
    let help = &bridge.state.outbox[0].text;
    for command in [
        "/help", "/recent", "/status", "/queue", "/cancel", "/unmute", "/update",
    ] {
        assert!(help.contains(command));
    }
    assert_eq!(
        help,
        "# codr commands\n\n### General\n\n**`/help`**  \nShow commands\n\n**`/status`**  \nConnection, state, queue and unmute\n\n**`/last [count]`**  \nReplay the newest message in this chat without unmuting. Use /last4 or /last 4 for four; count 1–100.\n\n**`/recent [hours]`**  \nGet newest 3 messages per chat active in the past hour. Use /recent2 or /recent 2 for two hours.\n\n**`/update`**  \nCheck ngit for a stable bridge update; confirm before installing\n\n### Projects & Agents\n\n**`/new`**  \nSet up a project\n\n**`/agent`**  \nChoose agent (separate chat)\n\n**`/model`**  \nChoose model\n\n### Tasks\n\n**`/queue`**  \nView pending deliveries\n\n**`/cancel`**  \nInterrupt the current task\n\n### Sync\n\n**`/unmute`**  \nUnmute all chats and return to automatic sync\n\n**`/sync auto|on|off`**  \nSet persistent sync mode\nCurrent sync mode: Automatic\n\nCommands accept unambiguous prefixes (/n, /st, /sy). In the control chat the leading / is optional."
    );
}

#[tokio::test]
async fn shortcut_sync_includes_a_recent_user_message_without_an_assistant_reply() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(
        &config,
        "ses_1",
        &[("user", "A prompt still awaiting its answer")],
        None,
    );
    let path = config.opencode_bin.with_file_name("ses_1.json");
    let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["messages"][0]["info"]["time"] = json!({"created":now()*1000});
    std::fs::write(path, data.to_string()).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    let mut event = inbound(HEX, "user-only-sync");
    event["message"]["text"] = json!("/recent");
    bridge.state.last_confirmed_send_at = Some(now());
    bridge.inbound(&event).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Recent: 1 chat queued.")
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.starts_with("**You:**\n_")
                && item.text.ends_with("\nA prompt still awaiting its answer"))
    );
}

/// Different /recent requests share delivery history, not just command replay guards.
#[tokio::test]
async fn shortcut_sync_does_not_resend_confirmed_or_pending_messages_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(
        &config,
        "ses_1",
        &[("user", "Question")],
        Some(("answer", "Answer")),
    );
    let path = config.opencode_bin.with_file_name("ses_1.json");
    let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["messages"][1]["info"]["time"]["completed"] = json!(now() * 1000);
    std::fs::write(&path, data.to_string()).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    let mut event = inbound(HEX, "dedup-first");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    let excerpt_count = |state: &BridgeState| {
        state
            .outbox
            .iter()
            .filter(|item| item.text.starts_with("**"))
            .count()
    };
    assert_eq!(excerpt_count(&bridge.state), 1);
    assert!(bridge.flush().await.is_err());
    assert!(
        bridge.state.delivered_messages.is_empty(),
        "Failed sends must not confirm source delivery"
    );
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    event["message"]["message_id_hex"] = json!("dedup-while-pending");
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        excerpt_count(&bridge.state),
        1,
        "Do not queue a second copy of pending source messages"
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    bridge.flush().await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    event["message"]["message_id_hex"] = json!("dedup-after-confirmation");
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(
        !bridge.state.outbox.iter().any(|item| item.text == "Answer"),
        "A later normal checkpoint must not forward the synced answer again"
    );
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        excerpt_count(&bridge.state),
        0,
        "Confirmed messages survive restart"
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Recent: nothing new.")
    );
    data["messages"].as_array_mut().unwrap().push(json!({"info":{"id":"new-user","role":"user","time":{"created":now()*1000}},"parts":[{"type":"text","text":"New question"}]}));
    std::fs::write(&path, data.to_string()).unwrap();
    event["message"]["message_id_hex"] = json!("dedup-overlap");
    bridge.inbound(&event).await.unwrap();
    let excerpts: Vec<_> = bridge
        .state
        .outbox
        .iter()
        .filter(|item| item.text.starts_with("**"))
        .collect();
    assert_eq!(excerpts.len(), 1);
    assert!(excerpts[0].text.starts_with("**You:**\n_"));
    assert!(excerpts[0].text.ends_with("\nNew question"));
    assert!(!excerpts[0].text.contains("\n\n---\n\n"));
    mdk.abort();
}

#[tokio::test]
async fn shortcut_sync_skips_assistant_messages_confirmed_by_automatic_forwarding() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(
        &config,
        "ses_1",
        &[("user", "Question")],
        Some(("answer", "Answer")),
    );
    let path = config.opencode_bin.with_file_name("ses_1.json");
    let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["messages"][1]["info"]["time"]["completed"] = json!(now() * 1000);
    std::fs::write(path, data.to_string()).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "send_final" && call["text"] == "Answer")
    );
    let mut event = inbound(HEX, "dedup-normal-final");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "**You:**\n_Date unknown_\nQuestion")
    );
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("**Agent:**"))
    );
    mdk.abort();
}

/// A reply written while the reader runs is current activity, not a future clock.
#[tokio::test]
async fn shortcut_sync_accepts_activity_written_during_its_read() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::write(&config.opencode_bin, r##"#!/bin/sh
sleep 2
printf '{"messages":[{"info":{"id":"reply","role":"assistant","time":{"completed":%s000},"finish":"stop"},"parts":[{"type":"text","text":"New during sync"}]}]}' "$(date +%s)"
"##).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    let mut event = inbound(HEX, "sync-during-reply");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.starts_with("**Agent:**\n_")
                && item.text.ends_with("\nNew during sync"))
    );
}

/// Explicit replay is local to one conversation and does not alter sync state.
#[tokio::test]
async fn last_replays_only_the_current_chat_without_unmuting_or_consuming_history() {
    for (command, count) in [("/last", 1), ("/last4", 4), ("/last 4", 4), ("/LAST 20", 5)] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let group = "bb".repeat(16);
        input_history(
            &config,
            "ses_1",
            &[
                ("one", "First"),
                ("two", "Second"),
                ("three", "Third"),
                ("four", "Fourth"),
            ],
            Some(("reply", "Fifth")),
        );
        input_history(
            &config,
            "ses_other",
            &[("other", "Other chat private text")],
            None,
        );
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let mut other = agent();
        other.agent_session.as_mut().unwrap().value = "ses_other".into();
        bridge
            .state
            .mappings
            .insert(agent().key(), activity_mapping(agent(), &group));
        bridge
            .state
            .mappings
            .insert(other.key(), activity_mapping(other, &"cc".repeat(16)));
        herdr_white_noise_bridge::forwarding::set_mode(
            &config.state_path,
            herdr_white_noise_bridge::forwarding::Mode::Off,
        )
        .unwrap();
        let control_before =
            std::fs::read(config.state_path.with_extension("forwarding.json")).ok();
        let history = herdr_white_noise_bridge::output::read_history(
            &config,
            agent().agent_session.as_ref().unwrap(),
        )
        .await
        .unwrap();
        let delivered: Vec<_> = history.recent_messages.iter().map(|m| m.key()).collect();
        bridge
            .state
            .delivered_messages
            .insert(group.clone(), delivered.clone());
        bridge.save().unwrap();
        let mut event = inbound(HEX, "last-request");
        event["message"]["text"] = json!(command);
        bridge.inbound(&event).await.unwrap();
        assert!(!bridge.state.forwarding_enabled);
        assert_eq!(bridge.state.delivered_messages[&group], delivered);
        assert_eq!(bridge.state.outbox.len(), 1);
        let reply = &bridge.state.outbox[0];
        assert_eq!(reply.group, group);
        assert!(!reply.text.contains("Other chat private text"));
        let expected = ["First", "Second", "Third", "Fourth", "Fifth"];
        for (i, text) in expected.iter().enumerate() {
            assert_eq!(
                reply.text.contains(text),
                i >= expected.len() - count,
                "{command}: {text}"
            );
        }
        if count > 1 {
            assert!(reply.text.find("Fourth").unwrap() < reply.text.find("Fifth").unwrap());
        }
        assert_eq!(
            std::fs::read(config.state_path.with_extension("forwarding.json")).ok(),
            control_before
        );
        drop(bridge);
        let mut bridge = Bridge::open(config).unwrap();
        bridge.inbound(&event).await.unwrap();
        assert_eq!(
            bridge.state.outbox.len(),
            1,
            "transport replay must deduplicate"
        );
        event["message"]["message_id_hex"] = json!("fresh-last-request");
        bridge.inbound(&event).await.unwrap();
        assert_eq!(
            bridge.state.outbox.len(),
            2,
            "a fresh explicit request may replay known text"
        );
        assert!(!bridge.state.forwarding_enabled);
    }
}

#[tokio::test]
async fn last_rejects_invalid_counts_and_nonstandalone_commands_without_waking() {
    for text in [
        "/last 0",
        "/last0",
        "/last -1",
        "/last 1.5",
        "/last 101",
        "/last 999999999999999999999999",
        "/last 4 extra",
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge
            .state
            .mappings
            .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
        herdr_white_noise_bridge::forwarding::set_mode(
            &config.state_path,
            herdr_white_noise_bridge::forwarding::Mode::Off,
        )
        .unwrap();
        let mut event = inbound(HEX, "invalid-last");
        event["message"]["text"] = json!(text);
        bridge.inbound(&event).await.unwrap();
        assert!(
            bridge.state.outbox[0].text.starts_with("Use /last"),
            "{text}"
        );
        assert!(!bridge.state.forwarding_enabled);
    }
    for field in [
        "text_truncated",
        "attachments_truncated",
        "attachments",
        "media",
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge
            .state
            .mappings
            .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
        let mut event = inbound(HEX, "attached-last");
        event["message"]["text"] = json!("/last4");
        event["message"][field] = if field.ends_with("truncated") {
            json!(true)
        } else {
            json!([{"media_type":"image/png"}])
        };
        bridge.inbound(&event).await.unwrap();
        assert!(bridge.state.outbox[0].text.contains("standalone"));
    }
}

#[tokio::test]
async fn last_reports_empty_unavailable_and_control_chats_without_agent_input() {
    for case in ["empty", "unavailable", "control"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        if case == "empty" {
            input_history(&config, "ses_1", &[], None);
        }
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let mut mapping = activity_mapping(agent(), &"bb".repeat(16));
        mapping.control = case == "control";
        bridge.state.mappings.insert(agent().key(), mapping);
        let mut event = inbound(HEX, "last-empty");
        event["message"]["text"] = json!("/last");
        bridge.inbound(&event).await.unwrap();
        let text = &bridge.state.outbox[0].text;
        assert!(
            text.contains(match case {
                "empty" => "No conversation",
                "unavailable" => "unavailable",
                _ => "agent chat",
            }),
            "{case}: {text}"
        );
        assert!(!bridge.state.mappings[&agent().key()].pending_prompt);
    }
}

#[tokio::test]
async fn last_manual_reply_is_delivered_while_sync_stays_off() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(
        &config,
        "ses_1",
        &[("question", "Before")],
        Some(("answer", "Replay this")),
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    herdr_white_noise_bridge::forwarding::set_mode(
        &config.state_path,
        herdr_white_noise_bridge::forwarding::Mode::Off,
    )
    .unwrap();
    let mut event = inbound(HEX, "last-send");
    event["message"]["text"] = json!("/last");
    bridge.inbound(&event).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    assert!(!bridge.state.forwarding_enabled);
    assert!(calls.lock().unwrap().iter().any(|call| {
        call["type"] == "send_final"
            && call["text"]
                .as_str()
                .is_some_and(|text| text.contains("Replay this"))
    }));
    mdk.abort();
}

#[tokio::test]
async fn last_failed_save_keeps_request_retryable_and_preserves_accepted_work() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(
        &config,
        "ses_1",
        &[("question", "Before")],
        Some(("answer", "Replay this")),
    );
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    bridge
        .state
        .mappings
        .insert(key.clone(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.state.mappings.get_mut(&key).unwrap().pending_prompt = true;
    accept_legacy_text(&mut bridge, &key, "accepted", "Keep accepted input");
    let accepted = serde_json::to_value(&bridge.state.mappings[&key].prompt_queue).unwrap();
    let durable = std::fs::read(&config.state_path).unwrap();
    std::fs::remove_file(&config.state_path).unwrap();
    std::fs::create_dir(&config.state_path).unwrap();
    let mut event = inbound(HEX, "last-save-failure");
    event["message"]["text"] = json!("/last");
    assert!(bridge.inbound(&event).await.is_err());
    assert!(bridge.state.outbox.is_empty());
    assert!(
        !bridge.state.mappings[&key]
            .seen_messages
            .iter()
            .any(|id| id == "last-save-failure")
    );
    assert!(bridge.state.mappings[&key].pending_prompt);
    assert_eq!(
        serde_json::to_value(&bridge.state.mappings[&key].prompt_queue).unwrap(),
        accepted
    );
    std::fs::remove_dir(&config.state_path).unwrap();
    std::fs::write(&config.state_path, durable).unwrap();
    bridge.inbound(&event).await.unwrap();
    assert_eq!(bridge.state.outbox.len(), 1);
    assert!(bridge.state.mappings[&key].pending_prompt);
    assert_eq!(
        serde_json::to_value(&bridge.state.mappings[&key].prompt_queue).unwrap(),
        accepted
    );
}

/// Hours change conversation eligibility, not the excerpt size or duplicate rules.
#[tokio::test]
async fn recent_optional_hours_select_the_window_without_changing_message_limit_or_sync() {
    for (command, hours) in [
        ("/recent", 1),
        ("/recent 2", 2),
        ("/recent2", 2),
        ("/recent 4", 4),
        ("/recent4", 4),
        ("/recent 24", 24),
        ("/recent24", 24),
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.state.last_confirmed_send_at = Some(now() - 48 * 3600);
        let cases = [
            ("ses_1", "bb".repeat(16), 1800),
            ("ses_2", "cc".repeat(16), 5400),
            ("ses_3", "dd".repeat(16), 3 * 3600),
            ("ses_4", "ee".repeat(16), 23 * 3600),
            ("ses_5", "ff".repeat(16), 25 * 3600),
        ];
        for (session, group, age) in &cases {
            let mut observed = agent();
            observed.agent_session.as_mut().unwrap().value = (*session).into();
            bridge
                .state
                .mappings
                .insert(observed.key(), activity_mapping(observed, group));
            input_history(
                &config,
                session,
                &[("one", "First"), ("two", "Second"), ("three", "Third")],
                Some(("reply", "Fourth")),
            );
            let path = config
                .opencode_bin
                .with_file_name(format!("{session}.json"));
            let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            data["messages"][0]["info"]["time"] = json!({"created":(now()-age)*1000});
            std::fs::write(path, data.to_string()).unwrap();
        }
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        bridge.save().unwrap();
        let mut event = inbound(HEX, "hours-request");
        event["message"]["text"] = json!(command);
        bridge.inbound(&event).await.unwrap();
        assert!(!bridge.state.forwarding_enabled);
        for (_, group, age) in &cases {
            let excerpts: Vec<_> = bridge
                .state
                .outbox
                .iter()
                .filter(|item| &item.group == group && item.text.starts_with("**You:**"))
                .collect();
            assert_eq!(
                excerpts.len(),
                usize::from(*age <= hours * 3600),
                "{command}, {age}"
            );
            for excerpt in excerpts {
                assert!(!excerpt.text.contains("First"));
                assert!(
                    excerpt.text.contains("Second")
                        && excerpt.text.contains("Third")
                        && excerpt.text.contains("Fourth")
                );
            }
        }
        let count = bridge
            .state
            .outbox
            .iter()
            .filter(|item| item.text.starts_with("**You:**"))
            .count();
        drop(bridge);
        let mut bridge = Bridge::open(config).unwrap();
        event["message"]["message_id_hex"] = json!("another-hours-request");
        bridge.inbound(&event).await.unwrap();
        assert_eq!(
            bridge
                .state
                .outbox
                .iter()
                .filter(|item| item.text.starts_with("**You:**"))
                .count(),
            count
        );
    }
}

#[tokio::test]
async fn recent_rejects_invalid_hours_without_prompting_or_waking_agents() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    for (i, command) in [
        "/recent0",
        "/recent 0",
        "/recent -1",
        "/recent 1.5",
        "/recent many",
        "/recent 2 4",
        "/recent2 extra",
        "/recent 18446744073709551615",
        "/recent9999999999999999999999999",
    ]
    .iter()
    .enumerate()
    {
        let mut event = inbound(HEX, &format!("invalid-hours-{i}"));
        event["message"]["text"] = json!(command);
        bridge.inbound(&event).await.unwrap();
        assert!(
            !herdr_white_noise_bridge::forwarding::load(&config.state_path)
                .unwrap()
                .enabled
        );
        assert!(
            bridge.state.mappings[&agent().key()]
                .prompt_queue
                .is_empty()
        );
    }
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .filter(|item| item.text.starts_with("Use /recent"))
            .count(),
        9
    );
}

#[tokio::test]
async fn recent_uses_the_past_hour_independently_of_the_persisted_send_clock() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let reference = now() - 6 * 3600;
    for (session, group, age) in [
        ("ses_1", "bb".repeat(16), 1800),
        ("ses_other", "dd".repeat(16), 2700),
        ("ses_old", "ee".repeat(16), 5400),
    ] {
        let mut observed = agent();
        observed.agent_session.as_mut().unwrap().value = session.into();
        bridge
            .state
            .mappings
            .insert(observed.key(), activity_mapping(observed, &group));
        input_history(
            &config,
            session,
            &[("user", "prompt")],
            Some(("reply", session)),
        );
        let path = config
            .opencode_bin
            .with_file_name(format!("{session}.json"));
        let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data["messages"][0]["info"]["time"] = json!({"created":(now()-age)*1000});
        std::fs::write(path, data.to_string()).unwrap();
    }
    let mut state = serde_json::to_value(&bridge.state).unwrap();
    state["last_confirmed_send_at"] = json!(reference);
    bridge.state = serde_json::from_value(state).unwrap();
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut event = inbound(HEX, "global-sync");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.group == "dd".repeat(16) && item.text.ends_with("\nses_other"))
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.group == "bb".repeat(16) && item.text.ends_with("\nses_1"))
    );
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.group == "ee".repeat(16))
    );
    assert_eq!(
        serde_json::to_value(&bridge.state).unwrap()["last_confirmed_send_at"],
        reference
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    bridge.flush().await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    assert!(
        serde_json::to_value(&bridge.state).unwrap()["last_confirmed_send_at"]
            .as_u64()
            .unwrap()
            >= now() - 5
    );
    event["message"]["message_id_hex"] = json!("next-global-sync");
    bridge.inbound(&event).await.unwrap();
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.starts_with("**You:**"))
    );
    mdk.abort();
}

/// Failed sends and unrelated inbound commands do not invent a confirmation clock.
#[tokio::test]
async fn sync_reference_advances_only_after_a_confirmed_send_in_any_chat() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .enqueue(&"dd".repeat(16), "reply in another conversation");
    bridge.save().unwrap();
    assert!(bridge.flush().await.is_err());
    assert!(serde_json::to_value(&bridge.state).unwrap()["last_confirmed_send_at"].is_null());
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls).await;
    bridge.flush().await.unwrap();
    assert!(
        serde_json::to_value(&bridge.state).unwrap()["last_confirmed_send_at"]
            .as_u64()
            .unwrap()
            >= now() - 5
    );
    mdk.abort();
}

/// Recover one global maximum from legacy history, paging past recent phone input.
#[tokio::test]
async fn sync_reference_bootstraps_legacy_state_from_global_own_message_history() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    for (session, group, age) in [
        ("ses_1", "bb".repeat(16), 1800),
        ("ses_other", "dd".repeat(16), 2700),
        ("ses_old", "ee".repeat(16), 5400),
    ] {
        let mut observed = agent();
        observed.agent_session.as_mut().unwrap().value = session.into();
        bridge
            .state
            .mappings
            .insert(observed.key(), activity_mapping(observed, &group));
        input_history(
            &config,
            session,
            &[("user", "prompt")],
            Some(("reply", session)),
        );
        let path = config
            .opencode_bin
            .with_file_name(format!("{session}.json"));
        let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data["messages"][0]["info"]["time"] = json!({"created":(now()-age)*1000});
        std::fs::write(path, data.to_string()).unwrap();
    }
    let mut legacy = serde_json::to_value(&bridge.state).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("last_confirmed_send_at");
    std::fs::write(&config.state_path, legacy.to_string()).unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let received = calls.clone();
    let reference = now() - 5 * 3600;
    let mdk = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let req = read_line(&mut reader).await.unwrap();
            received.lock().unwrap().push(req.clone());
            assert_eq!(req["type"], "timeline_list");
            let first_page = req["group_id_hex"] == "bb".repeat(16) && req["before"].is_null();
            let middle_page = req["group_id_hex"] == "bb".repeat(16)
                && req["before"]["message_id_hex"] == "phone-sync";
            let at = if first_page {
                reference - 3600
            } else if middle_page {
                reference - 1800
            } else if req["group_id_hex"] == "bb".repeat(16) {
                reference
            } else if req["group_id_hex"] == "dd".repeat(16) {
                reference - 3600
            } else {
                reference - 3 * 3600
            };
            let response = json!({"id":req["id"],"marmot_agent_control":"marmot.agent-control.v2","type":"timeline_page","has_more_before":first_page || middle_page,"messages":[{
                "message_id_hex":if first_page { "phone-sync" } else if middle_page { "middle-reply" } else { "own-reply" },"recorded_at":at,"kind":9,"availability":"available","sender":{"is_self":!first_page}
            }]});
            reader
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let mut event = inbound(HEX, "legacy-global-sync");
    event["message"]["text"] = json!("/recent");
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        serde_json::to_value(&bridge.state).unwrap()["last_confirmed_send_at"],
        reference
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.ends_with("\nses_1"))
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.ends_with("\nses_other"))
    );
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.ends_with("\nses_old"))
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|req| !req["before"].is_null())
    );
    mdk.abort();
}

/// Invalid acknowledgements cannot remove queued messages or advance the reference.
#[tokio::test]
async fn sync_reference_rejects_malformed_send_confirmations() {
    for ids in [json!([]), json!([null]), json!([""])] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.state.enqueue(&"bb".repeat(16), "reply");
        let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
        let mdk = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let req = read_line(&mut reader).await.unwrap();
            let response = json!({"id":req["id"],"marmot_agent_control":"marmot.agent-control.v2","type":"final_sent","message_ids_hex":ids});
            reader
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        assert!(bridge.flush().await.is_err());
        assert_eq!(bridge.state.last_confirmed_send_at, None);
        assert_eq!(bridge.state.outbox.len(), 1);
        mdk.await.unwrap();
    }
}

/// Confirmed progress creation counts as a new message; later edits do not.
#[tokio::test]
async fn sync_reference_records_new_progress_and_help_but_not_edits_or_inbound_requests() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls).await;
    bridge
        .state
        .begin_response(&agent().key(), &"bb".repeat(16));
    bridge.flush().await.unwrap();
    assert!(bridge.state.last_confirmed_send_at.unwrap() >= now() - 5);
    let previous = now() - 1800;
    bridge.state.last_confirmed_send_at = Some(previous);
    bridge.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    bridge.state.update_response(
        &agent().key(),
        &"bb".repeat(16),
        "> working...\nnew tasks",
        false,
    );
    bridge.flush().await.unwrap();
    assert_eq!(bridge.state.last_confirmed_send_at, Some(previous));
    let mut event = inbound(HEX, "help-does-not-anchor-until-sent");
    event["message"]["text"] = json!("/help");
    bridge.inbound(&event).await.unwrap();
    assert_eq!(bridge.state.last_confirmed_send_at, Some(previous));
    bridge.flush().await.unwrap();
    assert!(bridge.state.last_confirmed_send_at.unwrap() >= now() - 5);
    mdk.abort();
}

/// Neither shortcut can be invoked by a different account, sender, or chat.
#[tokio::test]
async fn shortcut_commands_preserve_owner_account_and_group_authorization() {
    for command in ["/recent", "/help", "/queue", "/clear-queue"] {
        for invalid in ["owner", "account", "group", "self"] {
            let root = tempfile::tempdir().unwrap();
            let config = config(root.path());
            let mut bridge = Bridge::open(config.clone()).unwrap();
            bridge
                .state
                .mappings
                .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
            let mut event = inbound(HEX, "not-authorized");
            event["message"]["text"] = json!(command);
            match invalid {
                "owner" => event["message"]["sender"]["account_id_hex"] = json!("22".repeat(32)),
                "account" => event["account_id_hex"] = json!("33".repeat(32)),
                "group" => event["group_id_hex"] = json!("44".repeat(16)),
                _ => event["message"]["sender"]["is_self"] = json!(true),
            }
            let before = bridge.state.clone();
            bridge.inbound(&event).await.unwrap();
            assert_eq!(bridge.state, before);
        }
    }
}

#[tokio::test]
async fn incomplete_input_in_an_ended_session_mutes_but_does_not_block_a_phone_wake() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.auto_forwarding = true;
    let first = agent();
    let mut other = first.clone();
    other.agent_session.as_mut().unwrap().value = "ses_other".into();
    other.pane_id = "w1:p2".into();
    other.terminal_id = "term2".into();
    input_history(&config, "ses_1", &[], None);
    input_history(&config, "ses_other", &[], None);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        first.key(),
        activity_mapping(first.clone(), &"bb".repeat(16)),
    );
    bridge.state.mappings.insert(
        other.key(),
        activity_mapping(other.clone(), &"dd".repeat(16)),
    );
    bridge
        .reconcile(vec![first.clone(), other.clone()])
        .await
        .unwrap();
    std::fs::write(
        config.opencode_bin.with_file_name("ses_1.json"),
        json!({"messages":[{"info":{"id":"incomplete","role":"user"},"parts":[]}]}).to_string(),
    )
    .unwrap();
    bridge.reconcile(vec![other.clone()]).await.unwrap();
    assert!(!bridge.state.forwarding_enabled);
    assert!(!bridge.state.mappings[&first.key()].active);
    assert!(!bridge.state.mappings[&first.key()].activity.unresolved());
    drop(bridge);
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), other.clone()).await;
    let mut reopened = Bridge::open(config).unwrap();
    reopened.reconcile(vec![other]).await.unwrap();
    let mut event = inbound(HEX, "wake-after-end");
    event["group_id_hex"] = json!("dd".repeat(16));
    reopened.inbound(&event).await.unwrap();
    assert!(reopened.state.forwarding_enabled);
    reopened
        .state
        .enqueue(&"dd".repeat(16), "Delivery is no longer blocked");
    reopened.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["text"] == "Delivery is no longer blocked")
    );
    mdk.abort();
    herdr.abort();
}

/// Persisted command replies survive send failures, restarts, and later mute changes.
#[tokio::test]
async fn shortcut_delivery_retries_the_same_idempotency_key_without_resending_history() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    let mut event = inbound(HEX, "offline-help");
    event["message"]["text"] = json!("/help");
    bridge.inbound(&event).await.unwrap();
    let queued = bridge.state.outbox[0].clone();
    assert!(bridge.flush().await.is_err());
    drop(bridge);
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    assert_eq!(bridge.state.outbox, vec![queued.clone()]);
    bridge.inbound(&event).await.unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    bridge.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["idempotency_key"], queued.id);
    assert!(!bridge.state.forwarding_enabled);
    assert!(bridge.state.outbox.is_empty());
    mdk.abort();
}

/// Only complete standalone text is a shortcut, never a caption or truncated message.
#[tokio::test]
async fn shortcut_commands_reject_captions_truncation_and_longer_text() {
    for command in ["/help", "/recent", "/recent2", "/recent 2"] {
        for invalid in [
            "caption",
            "media",
            "text_truncated",
            "attachments_truncated",
            "prose",
        ] {
            let root = tempfile::tempdir().unwrap();
            let config = config(root.path());
            let mut bridge = Bridge::open(config.clone()).unwrap();
            bridge
                .state
                .mappings
                .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
            let mut event = inbound(HEX, "not-a-shortcut");
            event["message"]["text"] = json!(command);
            match invalid {
                "caption" => event["message"]["attachments"] = json!([{"media_type":"image/png"}]),
                "media" => event["message"]["media"] = json!([{"media_type":"audio/ogg"}]),
                "prose" => event["message"]["text"] = json!(format!("{command} please")),
                field => event["message"][field] = json!(true),
            }
            if command != "/help" && invalid == "prose" {
                bridge.inbound(&event).await.unwrap();
                assert!(
                    bridge
                        .state
                        .outbox
                        .iter()
                        .any(|item| item.text.starts_with("Use /recent"))
                );
                continue;
            }
            assert!(bridge.inbound(&event).await.is_err());
            assert!(bridge.state.outbox.is_empty());
            assert!(
                bridge.state.mappings[&agent().key()]
                    .seen_messages
                    .is_empty()
            );
        }
    }
}

/// An owner command wakes every chat, but never starts a new agent turn.
#[tokio::test]
async fn unmute_wakes_all_chats_without_agent_input_and_replay_cannot_unmute_again() {
    for command in ["unmute", " /UnMuTe \n"] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.auto_forwarding = true;
        let first = agent();
        let mut other = first.clone();
        other.agent_session.as_mut().unwrap().value = "ses_other".into();
        input_history(&config, "ses_1", &[], None);
        input_history(&config, "ses_other", &[], None);
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        // No herdr server: the command must not query, prompt, or answer the agent.
        let mut bridge = Bridge::open(config.clone()).unwrap();
        for (observed, group) in [(&first, "bb".repeat(16)), (&other, "dd".repeat(16))] {
            bridge
                .state
                .mappings
                .insert(observed.key(), activity_mapping(observed.clone(), &group));
        }
        bridge
            .reconcile(vec![first.clone(), other.clone()])
            .await
            .unwrap();
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        bridge
            .reconcile(vec![first.clone(), other.clone()])
            .await
            .unwrap();
        assert!(!bridge.state.forwarding_enabled);
        input_history(
            &config,
            "ses_1",
            &[("local", "Local prompt")],
            Some(("old-final", "Muted final")),
        );
        let mut event = inbound(HEX, "unmute-command");
        event["message"]["text"] = json!(command);
        bridge.inbound(&event).await.unwrap();
        assert!(bridge.state.forwarding_enabled);
        assert!(!bridge.state.forwarding_resume_pending);
        assert!(
            bridge.state.mappings[&first.key()]
                .final_ids
                .contains(&"old-final".into())
        );
        assert!(
            bridge
                .state
                .mappings
                .values()
                .all(|mapping| !mapping.pending_prompt && !mapping.forwarding_baseline_pending)
        );
        assert_eq!(bridge.state.outbox.len(), 1);
        assert!(bridge.state.outbox[0].text.contains("all chats"));
        assert!(bridge.state.outbox[0].text.contains("No prompt was sent"));
        bridge.inbound(&event).await.unwrap();
        assert_eq!(
            bridge.state.outbox.len(),
            1,
            "Duplicate commands must not repeat acknowledgments"
        );
        bridge.state.enqueue(&"dd".repeat(16), "Other chat output");
        bridge.flush().await.unwrap();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call["text"] == "Other chat output")
        );
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        drop(bridge);
        let mut reopened = Bridge::open(config.clone()).unwrap();
        reopened.inbound(&event).await.unwrap();
        assert!(
            !herdr_white_noise_bridge::forwarding::load(&config.state_path)
                .unwrap()
                .enabled
        );
        assert!(reopened.state.outbox.is_empty());
        mdk.abort();
    }
}

/// Commands remain available in busy, blocked, and ended linked conversations.
#[tokio::test]
async fn unmute_is_not_an_agent_prompt_or_a_pending_question_answer() {
    for status in ["working", "blocked", "ended"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        export_script(&config.opencode_bin, "Existing reply", "old-final");
        let mut observed = agent();
        observed.agent_status = status.into();
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.state.mappings.insert(
            observed.key(),
            activity_mapping(observed.clone(), &"bb".repeat(16)),
        );
        bridge
            .reconcile(if status == "ended" {
                vec![]
            } else {
                vec![observed.clone()]
            })
            .await
            .unwrap();
        let mapping = bridge.state.mappings.get_mut(&observed.key()).unwrap();
        mapping.pending_prompt = status != "ended";
        mapping.decision = Some(
            serde_json::from_value(json!({"id":"question:tool", "announced":true,
            "questions":[{"question":"Which next step?","options":[{"label":"unmute"}]}]}))
            .unwrap(),
        );
        bridge.state.outbox.clear();
        let mut event = inbound(HEX, "command");
        event["message"]["text"] = json!("unmute");
        bridge.inbound(&event).await.unwrap();
        assert!(
            bridge.state.mappings[&observed.key()]
                .seen_messages
                .contains(&"command".into())
        );
        assert_eq!(
            bridge.state.mappings[&observed.key()].pending_prompt,
            status != "ended"
        );
        assert!(
            !bridge.state.mappings[&observed.key()]
                .decision
                .as_ref()
                .unwrap()
                .submitted
        );
        assert_eq!(bridge.state.outbox.len(), 1);
        assert!(bridge.state.outbox[0].text.contains("No prompt was sent"));
    }
}

/// Untrusted or unrelated events cannot change the forwarding mode.
#[tokio::test]
async fn unmute_keeps_owner_account_and_group_authorization() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            ..Default::default()
        },
    );
    for (field, value) in [
        ("sender", json!("22".repeat(32))),
        ("is_self", json!(true)),
        ("account", json!("33".repeat(32))),
        ("group", json!("dd".repeat(16))),
        ("type", json!("other_event")),
    ] {
        let mut event = inbound(HEX, field);
        event["message"]["text"] = json!("unmute");
        match field {
            "sender" => event["message"]["sender"]["account_id_hex"] = value,
            "is_self" => event["message"]["sender"]["is_self"] = value,
            "account" => event["account_id_hex"] = value,
            "group" => event["group_id_hex"] = value,
            _ => event["type"] = value,
        }
        bridge.inbound(&event).await.unwrap();
        assert!(
            !herdr_white_noise_bridge::forwarding::load(&config.state_path)
                .unwrap()
                .enabled,
            "{field}"
        );
        assert!(
            bridge.state.mappings[&agent().key()]
                .seen_messages
                .is_empty()
        );
        assert!(bridge.state.outbox.is_empty());
    }
}

/// Command-like prose is an ordinary prompt, not a bridge command.
#[tokio::test]
async fn unmute_requires_a_standalone_command() {
    for text in [
        "unmute please",
        "please unmute",
        "/unmute later",
        "unmuted",
        "/unknown",
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        export_script(&config.opencode_bin, "Existing reply", "old-final");
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
        let mut bridge = Bridge::open(config).unwrap();
        let observed = agent();
        bridge.state.mappings.insert(
            observed.key(),
            activity_mapping(observed.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![observed.clone()]).await.unwrap();
        bridge.state.outbox.clear();
        let mut event = inbound(HEX, "ordinary");
        event["message"]["text"] = json!(text);
        bridge.inbound(&event).await.unwrap();
        assert!(bridge.state.mappings[&observed.key()].pending_prompt);
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call["method"] == "agent.prompt" && call["params"]["text"] == text)
                .count(),
            1
        );
        assert!(
            bridge
                .state
                .outbox
                .iter()
                .all(|item| !item.text.contains("No prompt was sent"))
        );
        herdr.abort();
    }
}

/// Truncated input and media captions must not become control commands.
#[tokio::test]
async fn unmute_requires_complete_text_without_attachments() {
    for (field, value) in [
        ("text_truncated", json!(true)),
        ("attachments_truncated", json!(true)),
        ("media", json!([{"media_type":"image/png"}])),
        (
            "attachments",
            json!([{"media_type":"image/png","file_name":"picture.png"}]),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        export_script(&config.opencode_bin, "Existing reply", "old-final");
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = peer(&config.herdr_socket, false, calls).await;
        let mut bridge = Bridge::open(config).unwrap();
        let observed = agent();
        bridge.state.mappings.insert(
            observed.key(),
            activity_mapping(observed.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![observed]).await.unwrap();
        bridge.state.outbox.clear();
        let mut event = inbound(HEX, "not-a-command");
        event["message"]["text"] = json!("unmute");
        event["message"][field] = value;
        bridge.inbound(&event).await.unwrap();
        assert!(
            bridge
                .state
                .outbox
                .iter()
                .all(|item| !item.text.contains("enabled for all chats")),
            "{field}"
        );
        herdr.abort();
    }
}

/// Only typed commands are controls; a prepared voice transcript remains agent input.
#[tokio::test]
async fn command_prefixes_preserve_prepared_and_accepted_payloads() {
    for prepared in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        input_history(&config, "ses_1", &[], None);
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
        let mut bridge = Bridge::open(config).unwrap();
        let observed = agent();
        bridge.state.mappings.insert(
            observed.key(),
            activity_mapping(observed.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![observed.clone()]).await.unwrap();
        let text = if prepared { "/st" } else { "/h" };
        if prepared {
            let mut event = inbound(HEX, "prepared-prefix");
            event["message"]["text"] = json!("");
            event["message"]["media"] = json!([{"media_type":"audio/ogg"}]);
            event["_codr_prepared_text"] = json!(text);
            bridge.state.media_pending.push(event);
            bridge.process_media().await.unwrap();
        } else {
            accept_legacy_text(&mut bridge, &observed.key(), "accepted-prefix", text);
            bridge.reconcile(vec![observed.clone()]).await.unwrap();
        }
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| { call["method"] == "agent.prompt" && call["params"]["text"] == text }),
            "prepared={prepared}: accepted text must reach the agent unchanged"
        );
        assert!(
            bridge.state.mappings[&observed.key()]
                .prompt_queue
                .is_empty()
        );
        herdr.abort();
    }
}

#[tokio::test]
async fn unmute_in_a_voice_transcript_is_not_a_bridge_command() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    export_script(&config.opencode_bin, "Existing reply", "old-final");
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    let observed = agent();
    bridge.state.mappings.insert(
        observed.key(),
        activity_mapping(observed.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![observed.clone()]).await.unwrap();
    bridge.state.outbox.clear();
    let mut event = inbound(HEX, "voice");
    event["message"]["text"] = json!("");
    event["message"]["media"] = json!([{"media_type":"audio/ogg"}]);
    event["_codr_prepared_text"] = json!("unmute");
    bridge.state.media_pending.push(event);
    bridge.process_media().await.unwrap();
    assert!(bridge.state.media_pending.is_empty());
    assert!(bridge.state.mappings[&observed.key()].pending_prompt);
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt" && call["params"]["text"] == "unmute")
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .all(|item| !item.text.contains("enabled for all chats"))
    );
    herdr.abort();
}

#[tokio::test]
async fn a_local_user_message_mutes_every_group_and_drops_queued_output() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.auto_forwarding = true;
    let first = agent();
    let mut other = first.clone();
    other.agent_session.as_mut().unwrap().value = "ses_other".into();
    input_history(&config, "ses_1", &[("old", "Historical prompt")], None);
    input_history(
        &config,
        "ses_other",
        &[("old-other", "Other historical prompt")],
        None,
    );
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        first.key(),
        activity_mapping(first.clone(), &"bb".repeat(16)),
    );
    bridge.state.mappings.insert(
        other.key(),
        activity_mapping(other.clone(), &"dd".repeat(16)),
    );
    bridge
        .reconcile(vec![first.clone(), other.clone()])
        .await
        .unwrap();
    assert!(
        bridge.state.forwarding_enabled,
        "Initial history is only a baseline"
    );
    bridge
        .state
        .enqueue(&"dd".repeat(16), "Queued output from the other group");
    input_history(
        &config,
        "ses_1",
        &[
            ("old", "Historical prompt"),
            ("local", "Work entered in herdr"),
        ],
        Some(("answer", "Local work must not be forwarded")),
    );
    let mut changed = first.clone();
    changed.state_change_seq = 2;
    bridge.reconcile(vec![changed, other]).await.unwrap();
    assert!(!bridge.state.forwarding_enabled);
    assert!(bridge.state.outbox.is_empty());
    assert!(
        !herdr_white_noise_bridge::forwarding::load(&config.state_path)
            .unwrap()
            .enabled
    );
    drop(bridge);
    assert!(!Bridge::open(config).unwrap().state.forwarding_enabled);
}

#[tokio::test]
async fn phone_input_wakes_all_groups_and_its_native_echo_does_not_mute_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.auto_forwarding = true;
    let first = agent();
    let mut other = first.clone();
    other.agent_session.as_mut().unwrap().value = "ses_other".into();
    input_history(&config, "ses_1", &[("old", "Historical prompt")], None);
    input_history(&config, "ses_other", &[], None);
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        first.key(),
        activity_mapping(first.clone(), &"bb".repeat(16)),
    );
    bridge.state.mappings.insert(
        other.key(),
        activity_mapping(other.clone(), &"dd".repeat(16)),
    );
    bridge
        .reconcile(vec![first.clone(), other.clone()])
        .await
        .unwrap();
    input_history(
        &config,
        "ses_1",
        &[("old", "Historical prompt"), ("local", "Local prompt")],
        None,
    );
    bridge
        .reconcile(vec![first.clone(), other.clone()])
        .await
        .unwrap();
    assert!(!bridge.state.forwarding_enabled);
    let event = inbound(HEX, "phone-handoff");
    bridge.inbound(&event).await.unwrap();
    assert!(bridge.state.forwarding_enabled);
    drop(bridge);
    let transcript = config.opencode_bin.with_file_name("ses_1.json");
    let mut incomplete: Value =
        serde_json::from_slice(&std::fs::read(&transcript).unwrap()).unwrap();
    incomplete["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"info":{"id":"phone","role":"user"},"parts":[]}));
    std::fs::write(&transcript, incomplete.to_string()).unwrap();
    let mut reopened = Bridge::open(config.clone()).unwrap();
    reopened
        .reconcile(vec![first.clone(), other.clone()])
        .await
        .unwrap();
    assert!(reopened.state.forwarding_enabled);
    assert!(reopened.state.mappings[&first.key()].activity.unresolved());
    reopened
        .state
        .enqueue(&"dd".repeat(16), "Hold while user text is incomplete");
    let before_flush = calls.lock().unwrap().len();
    reopened.flush().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), before_flush);
    input_history(
        &config,
        "ses_1",
        &[
            ("old", "Historical prompt"),
            ("local", "Local prompt"),
            ("phone", "test prompt"),
        ],
        Some(("phone-reply", "Reply to the phone")),
    );
    let mut changed = first.clone();
    changed.state_change_seq = 3;
    reopened
        .reconcile(vec![changed.clone(), other.clone()])
        .await
        .unwrap();
    assert!(reopened.state.forwarding_enabled);
    reopened.inbound(&event).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    assert!(
        reopened
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Reply to the phone")
    );
    // The same text typed locally later is not another phone-origin prompt.
    input_history(
        &config,
        "ses_1",
        &[
            ("old", "Historical prompt"),
            ("local", "Local prompt"),
            ("phone", "test prompt"),
            ("local-again", "test prompt"),
        ],
        None,
    );
    changed.state_change_seq = 4;
    reopened.reconcile(vec![changed, other]).await.unwrap();
    assert!(!reopened.state.forwarding_enabled);
    mdk.abort();
    herdr.abort();
}

/// Characterize the screenshot's failure path without sending to live agents.
/// The peer acknowledges submission; the fixture supplies the resulting transcript.
#[tokio::test]
async fn phone_echo_concatenation_controls_muting_and_late_final_delivery() {
    const PHONE: &str = "This is just a test return with hello.";
    const COMBINED: &str = "An unsent desktop draft. This is just a test return with hello.";
    for v2 in [false, true] {
        for restart in [false, true] {
            for concatenated in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let mut config = config(root.path());
                config.auto_forwarding = true;
                config.edit_responses = true;
                let observed = agent();
                let group = "bb".repeat(16);
                let write_history = |echo: Option<&str>, completed: bool| {
                    let mut inputs = vec![("old", "Historical prompt")];
                    if let Some(text) = echo {
                        inputs.push(("phone", text));
                    }
                    input_history(
                        &config,
                        "ses_1",
                        &inputs,
                        completed.then_some(("hello-final", "Hello.")),
                    );
                    if v2 {
                        let mut messages: Vec<_> = inputs
                            .iter()
                            .map(|(id, text)| json!({"id":id,"type":"user","text":text}))
                            .collect();
                        if completed {
                            messages.push(json!({"id":"hello-final","type":"assistant","finish":"stop","time":{"completed":1},"content":[{"type":"text","text":"Hello."}]}));
                        }
                        std::fs::write(
                            config.opencode_bin.with_file_name("ses_1.json"),
                            json!({"messages":messages}).to_string(),
                        )
                        .unwrap();
                    }
                };
                write_history(None, false);
                let calls = Arc::new(Mutex::new(vec![]));
                let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
                let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
                let mut bridge = Bridge::open(config.clone()).unwrap();
                bridge
                    .state
                    .mappings
                    .insert(observed.key(), activity_mapping(observed.clone(), &group));
                bridge.reconcile(vec![observed.clone()]).await.unwrap();
                let mut event = inbound(HEX, "phone-draft-test");
                event["message"]["text"] = json!(PHONE);
                bridge.inbound(&event).await.unwrap();
                bridge.flush().await.unwrap();
                assert!(
                    calls
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|c| c["type"] == "send_final" && c["text"] == "> working...")
                );
                assert_eq!(
                    calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|c| c["method"] == "agent.prompt")
                        .count(),
                    1
                );
                assert!(
                    calls
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|c| c["method"] == "agent.prompt" && c["params"]["text"] == PHONE)
                );
                if restart {
                    drop(bridge);
                    bridge = Bridge::open(config.clone()).unwrap();
                }
                // Idle can arrive before the completed assistant transcript record.
                write_history(Some(if concatenated { COMBINED } else { PHONE }), false);
                let mut idle = observed.clone();
                idle.state_change_seq = 2;
                bridge.reconcile(vec![idle.clone()]).await.unwrap();
                bridge.flush().await.unwrap();
                assert_eq!(bridge.state.forwarding_enabled, !concatenated);
                assert!(!calls.lock().unwrap().iter().any(|c| c["text"] == "Hello."));
                assert!(!bridge.state.outbox.iter().any(|item| item.text == "> done"));
                assert!(
                    !calls
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|c| c["type"] == "edit_message" && c["text"] == "> done")
                );
                write_history(Some(if concatenated { COMBINED } else { PHONE }), true);
                // No new agent state event accompanies the late transcript record.
                // Muted turns have no pending response, so checkpoint on the next
                // periodic export; exact phone replies must not need that timeout.
                if concatenated {
                    bridge
                        .state
                        .mappings
                        .get_mut(&observed.key())
                        .unwrap()
                        .last_export_at = 0;
                }
                bridge.reconcile(vec![idle.clone()]).await.unwrap();
                // Advance the persisted edit cooldown without wall-clock sleeps.
                for item in &mut bridge.state.outbox {
                    if let Some(delivery) = item.response.as_mut() {
                        delivery.last_attempt_at = 0;
                    }
                }
                bridge.flush().await.unwrap();
                assert!(
                    bridge.state.mappings[&observed.key()]
                        .final_ids
                        .iter()
                        .any(|id| id == "hello-final")
                );
                assert_eq!(bridge.state.forwarding_enabled, !concatenated);
                let sent = calls.lock().unwrap().clone();
                assert_eq!(
                    sent.iter()
                        .filter(|c| c["type"] == "send_final"
                            && c["text"] == "Hello."
                            && c["group_id_hex"] == group)
                        .count(),
                    usize::from(!concatenated)
                );
                assert_eq!(
                    sent.iter()
                        .filter(|c| c["type"] == "edit_message" && c["text"] == "> done")
                        .count(),
                    usize::from(!concatenated)
                );
                // The consumed phone request must never prompt again after recovery.
                drop(bridge);
                let mut reopened = Bridge::open(config.clone()).unwrap();
                reopened.reconcile(vec![idle]).await.unwrap();
                reopened.inbound(&event).await.unwrap();
                reopened.flush().await.unwrap();
                assert_eq!(
                    calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|c| c["method"] == "agent.prompt")
                        .count(),
                    1
                );
                assert_eq!(
                    calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|c| c["type"] == "send_final" && c["text"] == "Hello.")
                        .count(),
                    usize::from(!concatenated)
                );
                assert_eq!(reopened.state.forwarding_enabled, !concatenated);
                herdr.abort();
                mdk.abort();
            }
        }
    }
}

#[tokio::test]
async fn legacy_prompt_queue_is_fifo_persistent_and_waits_for_a_completed_reply() {
    for edited_progress in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.edit_responses = edited_progress;
        input_history(&config, "ses_1", &[], None);
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
        let mut working = agent();
        working.agent_status = "working".into();
        working.state_change_seq = 2;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.state.mappings.insert(
            working.key(),
            activity_mapping(working.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![working.clone()]).await.unwrap();
        for (id, text) in [
            ("queued-first", "First task"),
            ("queued-second", "Second task"),
        ] {
            accept_legacy_text(&mut bridge, &agent().key(), id, text);
        }
        let acknowledgements: Vec<_> = bridge
            .state
            .outbox
            .iter()
            .filter(|item| item.manual && item.text.starts_with("Queued prompt at position "))
            .map(|item| item.text.as_str())
            .collect();
        assert!(
            acknowledgements.is_empty(),
            "An upgrade must not invent new acceptance acknowledgements for saved deliveries"
        );
        assert_eq!(
            serde_json::to_value(&bridge.state.mappings[&working.key()]).unwrap()["prompt_queue"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call["method"] == "agent.prompt")
        );
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![working]).await.unwrap();
        let mut idle = agent();
        idle.state_change_seq = 3;
        bridge.reconcile(vec![idle.clone()]).await.unwrap();
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call["method"] == "agent.prompt"),
            "The prior task's final must be collected before dispatch"
        );
        input_history(
            &config,
            "ses_1",
            &[("prior-input", "Prior task")],
            Some(("prior-final", "Prior answer")),
        );
        bridge.reconcile(vec![idle.clone()]).await.unwrap();
        let prompts = || {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call["method"] == "agent.prompt")
                .map(|call| call["params"]["text"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(prompts(), ["First task"]);
        bridge.reconcile(vec![idle.clone()]).await.unwrap();
        assert_eq!(
            prompts(),
            ["First task"],
            "A stale idle snapshot is not completion"
        );
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![idle.clone()]).await.unwrap();
        assert_eq!(
            prompts(),
            ["First task"],
            "Restart must not resubmit an in-flight task"
        );
        input_history(
            &config,
            "ses_1",
            &[("first-input", "First task")],
            Some(("first-final", "First answer")),
        );
        bridge.reconcile(vec![idle]).await.unwrap();
        assert_eq!(prompts(), ["First task", "Second task"]);
        herdr.abort();
    }
}

#[tokio::test]
async fn phone_delivery_is_immediate_while_accepted_porter_delivery_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![agent()]).await.unwrap();
    let request = json!({"version":1,"method":"enqueue","bridge_account":config.account_id_hex,
        "request_id":"00112233-4455-4677-8899-aabbccddeeff", "session_key":agent().key(),
        "terminal_id":agent().terminal_id, "text":"Desktop task"});
    herdr_white_noise_bridge::porter::handle(&mut bridge, request.clone())
        .await
        .unwrap();
    let mut phone = inbound(HEX, "phone-after-desktop");
    phone["message"]["text"] = json!("Phone task");
    bridge.inbound(&phone).await.unwrap();
    let prompts = || {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .map(|call| call["params"]["text"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(prompts(), ["Phone task"]);
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    assert_eq!(
        herdr_white_noise_bridge::porter::handle(&mut bridge, request)
            .await
            .unwrap()["status"],
        "already_accepted"
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(prompts(), ["Phone task"]);
    input_history(
        &config,
        "ses_1",
        &[("phone-input", "Phone task")],
        Some(("phone-final", "Phone answer")),
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(prompts(), ["Phone task", "Desktop task"]);
    herdr.abort();
}

#[tokio::test]
async fn retired_clear_queue_keeps_existing_deliveries_without_waking_or_prompting() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let calls = Arc::new(Mutex::new(vec![]));
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut working = agent();
    working.agent_status = "working".into();
    bridge.state.mappings.insert(
        working.key(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![working]).await.unwrap();
    accept_legacy_text(
        &mut bridge,
        &agent().key(),
        "queued-task",
        "Accepted before upgrade",
    );
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    for (id, text) in [
        ("inspect-queue", "/queue"),
        ("cancel-queue", "/clear-queue"),
    ] {
        let mut event = inbound(HEX, id);
        event["message"]["text"] = json!(text);
        bridge.inbound(&event).await.unwrap();
    }
    assert!(!bridge.state.forwarding_enabled);
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    assert_eq!(
        bridge.state.mappings[&agent().key()].prompt_queue[0].text,
        "Accepted before upgrade"
    );
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    bridge.flush().await.unwrap();
    assert!(calls.lock().unwrap().iter().any(|call| {
        call["text"]
            .as_str()
            .is_some_and(|text| text.contains("/clear-queue has been retired"))
    }));
    mdk.abort();
}

#[tokio::test]
async fn prompt_queue_pauses_when_muted_or_ended_and_resumes_only_the_same_session() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut mapping = activity_mapping(agent(), &"bb".repeat(16));
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "queued".into(),
            group: "bb".repeat(16),
            text: "Retained task".into(),
            media: None,
        });
    mapping.seen_messages.push("queued".into());
    bridge.state.mappings.insert(agent().key(), mapping);
    bridge.save().unwrap();
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.reconcile(vec![]).await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt")
    );
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    let mut other = agent();
    other.agent_session.as_mut().unwrap().value = "ses_other".into();
    input_history(&config, "ses_other", &[], None);
    herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
    bridge.reconcile(vec![other]).await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt")
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    herdr.abort();
}

#[tokio::test]
async fn prompt_queue_uncertain_submission_is_never_retried_after_restart_or_a_final() {
    for prepared_image in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        input_history(&config, "ses_1", &[], None);
        let calls = Arc::new(Mutex::new(vec![]));
        let listener = UnixListener::bind(&config.herdr_socket).unwrap();
        let received = calls.clone();
        let herdr = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let req = read_line(&mut reader).await.unwrap();
                received.lock().unwrap().push(req.clone());
                if req["method"] == "agent.prompt" {
                    continue;
                } // Submission may have happened; acknowledgement is lost.
                let response = json!({"id":req["id"],"result":{"agent":agent()}});
                reader
                    .get_mut()
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let mut mapping = activity_mapping(agent(), &"bb".repeat(16));
        for id in ["first", "second"] {
            mapping
                .prompt_queue
                .push(herdr_white_noise_bridge::state::QueuedPrompt {
                    id: id.into(),
                    group: "bb".repeat(16),
                    text: id.into(),
                    media: (prepared_image && id == "first")
                        .then(|| json!({"_codr_prepared_text":id})),
                });
            mapping.seen_messages.push(id.into());
        }
        bridge.state.mappings.insert(agent().key(), mapping);
        bridge.reconcile(vec![agent()]).await.unwrap();
        assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
        assert!(
            !bridge.state.mappings[&agent().key()]
                .queue_flight
                .as_ref()
                .unwrap()
                .confirmed
        );
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        input_history(
            &config,
            "ses_1",
            &[("user", "first")],
            Some(("reply", "Possibly submitted")),
        );
        let mut idle = agent();
        idle.state_change_seq = 2;
        bridge.reconcile(vec![idle.clone()]).await.unwrap();
        bridge.reconcile(vec![idle]).await.unwrap();
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call["method"] == "agent.prompt")
                .count(),
            1
        );
        assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
        let mut clear = inbound(HEX, "clear-waiting-only");
        clear["message"]["text"] = json!("/clear-queue");
        bridge.inbound(&clear).await.unwrap();
        assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
        assert!(
            !bridge.state.mappings[&agent().key()]
                .queue_flight
                .as_ref()
                .unwrap()
                .confirmed
        );
        herdr.abort();
    }
}

#[tokio::test]
async fn prompt_queue_restores_head_when_submission_is_known_not_to_have_started() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let listener = UnixListener::bind(&config.herdr_socket).unwrap();
    let path = config.herdr_socket.clone();
    let preflight = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let req = read_line(&mut reader).await.unwrap();
        assert_eq!(req["method"], "agent.get");
        std::fs::remove_file(path).unwrap();
        let response = json!({"id":req["id"],"result":{"agent":agent()}});
        reader
            .get_mut()
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
    });
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut mapping = activity_mapping(agent(), &"bb".repeat(16));
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "retry-head".into(),
            group: "bb".repeat(16),
            text: "Retained task".into(),
            media: None,
        });
    mapping.seen_messages.push("retry-head".into());
    bridge.state.mappings.insert(agent().key(), mapping);
    bridge.reconcile(vec![agent()]).await.unwrap();
    preflight.await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].queue_flight.is_none());
    assert!(!bridge.state.mappings[&agent().key()].pending_prompt);
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    drop(bridge);
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    herdr.abort();
}

#[tokio::test]
async fn direct_phone_delivery_keeps_owner_checks_and_deduplication_without_a_waiting_limit() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut working = agent();
    working.agent_status = "working".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer_for_agent(&config.herdr_socket, false, calls.clone(), working.clone()).await;
    bridge.state.mappings.insert(
        working.key(),
        activity_mapping(working.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![working]).await.unwrap();
    bridge
        .inbound(&inbound(&"22".repeat(32), "foreign"))
        .await
        .unwrap();
    for i in 0..18 {
        let event = inbound(HEX, &format!("waiting-{i}"));
        bridge.inbound(&event).await.unwrap();
        bridge.inbound(&event).await.unwrap();
    }
    for command in ["/help", "/queue", "/status", "/unmute"] {
        let mut event = inbound(HEX, command);
        event["message"]["text"] = json!(command);
        bridge.inbound(&event).await.unwrap();
    }
    assert!(
        bridge.state.mappings[&agent().key()]
            .prompt_queue
            .is_empty()
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        18
    );
    server.abort();
}

#[tokio::test]
async fn prompt_queue_background_checkpoint_never_starts_a_second_blocking_export() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let release = root.path().join("release");
    let slow_started = root.path().join("slow-started");
    let first = root.path().join("first-read");
    std::fs::write(&config.opencode_bin, format!("#!/bin/sh\nif [ \"$1\" = api ]; then exit 2; fi\nif [ -e {first:?} ]; then touch {slow_started:?}; while [ ! -e {release:?} ]; do sleep 0.01; done; else touch {first:?}; fi\nprintf '%s' '{{\"messages\":[]}}'\n")).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mapping = activity_mapping(agent(), &"bb".repeat(16));
    bridge.state.mappings.insert(agent().key(), mapping);
    for _ in 0..100 {
        bridge.poll_sessions(vec![agent()]).await.unwrap();
        if bridge.state.mappings[&agent().key()].last_export_at > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(bridge.state.mappings[&agent().key()].last_export_at > 0);
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "queued".into(),
            group: "bb".repeat(16),
            text: "Next task".into(),
            media: None,
        });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        bridge.poll_sessions(vec![agent()]),
    )
    .await;
    if result.is_err() {
        std::fs::write(&release, "release").unwrap();
    }
    assert!(
        result.is_ok(),
        "Queue dispatch blocked on a second synchronous transcript export"
    );
    for _ in 0..100 {
        if slow_started.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(slow_started.exists());
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt")
    );
    // A queued dispatch checkpoint can remain slow while commands and delivery run.
    let mut help = inbound(HEX, "help-during-slow-queue-read");
    help["message"]["text"] = json!("/help");
    tokio::time::timeout(std::time::Duration::from_secs(1), bridge.inbound(&help))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), bridge.flush())
        .await
        .unwrap()
        .unwrap();
    assert!(calls.lock().unwrap().iter().any(|call| {
        call["text"]
            .as_str()
            .is_some_and(|text| text.starts_with("# codr commands\n"))
    }));
    std::fs::write(release, "release").unwrap();
    for _ in 0..100 {
        bridge.poll_sessions(vec![agent()]).await.unwrap();
        if calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    herdr.abort();
    mdk.abort();
}

#[tokio::test]
async fn prompt_queue_dispatches_with_background_readers_and_no_new_idle_event() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.edit_responses = true;
    input_history(&config, "ses_1", &[], None);
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut mapping = activity_mapping(agent(), &"bb".repeat(16));
    for id in ["first", "second"] {
        mapping
            .prompt_queue
            .push(herdr_white_noise_bridge::state::QueuedPrompt {
                id: id.into(),
                group: "bb".repeat(16),
                text: id.into(),
                media: None,
            });
        mapping.seen_messages.push(id.into());
    }
    bridge.state.mappings.insert(agent().key(), mapping);
    for _ in 0..100 {
        bridge.poll_sessions(vec![agent()]).await.unwrap();
        if calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    input_history(
        &config,
        "ses_1",
        &[("user", "first")],
        Some(("first-final", "First reply")),
    );
    for _ in 0..100 {
        bridge.poll_sessions(vec![agent()]).await.unwrap();
        if calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count()
            == 2
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        2
    );
    herdr.abort();
}

#[tokio::test]
async fn prompt_queue_busy_chat_does_not_block_another_chats_queue() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    input_history(&config, "ses_1", &[], None);
    input_history(&config, "ses_other", &[], None);
    let mut working = agent();
    working.agent_status = "working".into();
    let mut idle = agent();
    idle.pane_id = "w1:p2".into();
    idle.terminal_id = "term2".into();
    idle.agent_session.as_mut().unwrap().value = "ses_other".into();
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), idle.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    for (agent, group) in [(&working, "bb".repeat(16)), (&idle, "dd".repeat(16))] {
        let mut mapping = activity_mapping(agent.clone(), &group);
        mapping
            .prompt_queue
            .push(herdr_white_noise_bridge::state::QueuedPrompt {
                id: agent.pane_id.clone(),
                group,
                text: agent.pane_id.clone(),
                media: None,
            });
        bridge.state.mappings.insert(agent.key(), mapping);
    }
    bridge
        .reconcile(vec![working.clone(), idle.clone()])
        .await
        .unwrap();
    let prompts: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call["method"] == "agent.prompt")
        .cloned()
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["params"]["target"], idle.pane_id);
    assert_eq!(bridge.state.mappings[&working.key()].prompt_queue.len(), 1);
    herdr.abort();
}

#[tokio::test]
async fn background_reads_deliver_a_fast_session_and_accept_input_while_another_is_slow() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let release = root.path().join("release");
    let started = root.path().join("slow-started");
    let data = json!({"messages":[{"info":{"id":"new-final","role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":"Fast answer"}]}]});
    std::fs::write(&config.opencode_bin, format!("#!/bin/sh\nif [ \"$3\" = ses_slow ]; then touch {started:?}; while [ ! -e {release:?} ]; do sleep 0.01; done; fi\nprintf '%s' '{data}'\n")).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    let fast = agent();
    let mut slow = fast.clone();
    slow.agent_session.as_mut().unwrap().value = "ses_slow".into();
    slow.pane_id = "w1:p2".into();
    for (a, group) in [(&fast, "bb".repeat(16)), (&slow, "dd".repeat(16))] {
        bridge.state.mappings.insert(
            a.key(),
            Mapping {
                group_id_hex: Some(group),
                output_initialised: true,
                active: true,
                agent_record: Some(a.clone()),
                last_agent_status: "idle".into(),
                last_state_change_seq: 1,
                ..Mapping::default()
            },
        );
    }
    let agents = vec![slow, fast.clone()];
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bridge.poll_sessions(agents.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    for _ in 0..1000 {
        bridge.poll_sessions(agents.clone()).await.unwrap();
        if started.exists()
            && bridge
                .state
                .outbox
                .iter()
                .any(|item| item.group == "bb".repeat(16) && item.text == "Fast answer")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(started.exists(), "The slow export must still be in flight");
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.group == "bb".repeat(16) && item.text == "Fast answer")
    );
    bridge.flush().await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bridge.inbound(&inbound(HEX, "phone-during-export")),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        bridge.state.mappings[&fast.key()]
            .seen_messages
            .contains(&"phone-during-export".into())
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    std::fs::write(release, "").unwrap();
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn completion_wakes_keep_uncertain_sessions_active_and_do_not_restart_working_exports() {
    let root = tempfile::tempdir().unwrap();
    let config = editable_config(root.path());
    let starts = root.path().join("starts");
    std::fs::write(
        &config.opencode_bin,
        format!(
            "#!/bin/sh\nif [ \"$1\" = api ]; then exit 2; fi\nprintf '%s\\n' \"$3\" >> {starts:?}\nprintf '%s' '{{\"messages\":[]}}'\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    let mut uncertain = agent();
    let mut agents = Vec::new();
    for index in 0..3 {
        let mut a = agent();
        a.pane_id = format!("w1:p{index}");
        a.terminal_id = format!("term{index}");
        a.agent_session.as_mut().unwrap().value = format!("ses_{index}");
        a.agent_status = "working".into();
        bridge.state.mappings.insert(
            a.key(),
            Mapping {
                group_id_hex: Some(format!("{index:032x}")),
                output_initialised: true,
                active: true,
                agent_record: Some(a.clone()),
                last_agent_status: "working".into(),
                last_state_change_seq: 1,
                ..Mapping::default()
            },
        );
        if index == 0 {
            uncertain = a.clone();
            a.agent_session = None;
        }
        agents.push(a);
    }
    bridge.poll_sessions(agents.clone()).await.unwrap();
    for _ in 0..100 {
        bridge.poll_ready_sessions().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(bridge.state.mappings[&uncertain.key()].active);
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.starts_with("> ended"))
    );
    assert_eq!(
        std::fs::read_to_string(&starts).unwrap().lines().count(),
        2,
        "Read completion must not trigger the next export before a scheduled poll"
    );
}

#[tokio::test]
async fn an_ended_session_requires_a_post_exit_snapshot_before_it_is_closed() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let source = root.path().join("export.json");
    let started = root.path().join("started");
    let release = root.path().join("release");
    std::fs::write(&source, "{\"messages\":[]}").unwrap();
    std::fs::write(&config.opencode_bin, format!("#!/bin/sh\ndata=$(cat {source:?})\ntouch {started:?}\nwhile [ ! -e {release:?} ]; do sleep 0.01; done\nprintf '%s' \"$data\"\n")).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    let a = agent();
    bridge.state.mappings.insert(
        a.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            output_initialised: true,
            active: true,
            agent_record: Some(a.clone()),
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    bridge.poll_sessions(vec![a.clone()]).await.unwrap();
    for _ in 0..1000 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(started.exists());
    std::fs::write(&source, json!({"messages":[{"info":{"id":"last-final","role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":"Final after exit"}]}]}).to_string()).unwrap();
    bridge.poll_sessions(vec![]).await.unwrap();
    std::fs::write(release, "").unwrap();
    for _ in 0..1000 {
        bridge.poll_sessions(vec![]).await.unwrap();
        if !bridge.state.mappings[&a.key()].active {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!bridge.state.mappings[&a.key()].active);
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Final after exit")
    );
}

#[tokio::test]
async fn background_export_failure_still_announces_a_blocked_session() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::write(&config.opencode_bin, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    let mut a = agent();
    bridge.state.mappings.insert(
        a.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            output_initialised: true,
            active: true,
            agent_record: Some(a.clone()),
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    a.agent_status = "blocked".into();
    a.state_change_seq = 2;
    for _ in 0..1000 {
        bridge.poll_sessions(vec![a.clone()]).await.unwrap();
        if bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("Agent needs input in the terminal"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("Agent needs input in the terminal"))
    );
}

#[tokio::test]
async fn background_task_export_failure_does_not_hide_working_status() {
    let root = tempfile::tempdir().unwrap();
    let config = editable_config(root.path());
    std::fs::write(&config.opencode_bin, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    let mut a = agent();
    bridge.state.mappings.insert(
        a.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            output_initialised: true,
            active: true,
            agent_record: Some(a.clone()),
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    a.agent_status = "working".into();
    a.state_change_seq = 2;
    for _ in 0..1000 {
        bridge.poll_sessions(vec![a.clone()]).await.unwrap();
        if bridge.state.mappings[&a.key()].last_agent_status == "working" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(bridge.state.mappings[&a.key()].last_agent_status, "working");
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "> working...")
    );
}

#[tokio::test]
async fn background_reads_discard_a_snapshot_from_an_older_agent_state() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let source = root.path().join("export.json");
    let started = root.path().join("started");
    let release = root.path().join("release");
    let data = |id: &str, text: &str| {
        json!({"messages":[{"info":{"id":id,"role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":text}]}]}).to_string()
    };
    std::fs::write(&source, data("stale", "Stale final")).unwrap();
    std::fs::write(&config.opencode_bin, format!("#!/bin/sh\ndata=$(cat {source:?})\ntouch {started:?}\nwhile [ ! -e {release:?} ]; do sleep 0.01; done\nprintf '%s' \"$data\"\n")).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    let mut a = agent();
    bridge.state.mappings.insert(
        a.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            output_initialised: true,
            active: true,
            agent_record: Some(a.clone()),
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    bridge.poll_sessions(vec![a.clone()]).await.unwrap();
    for _ in 0..1000 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(started.exists());
    a.state_change_seq = 2;
    std::fs::write(&source, data("current", "Current final")).unwrap();
    std::fs::write(release, "").unwrap();
    for _ in 0..1000 {
        bridge.poll_sessions(vec![a.clone()]).await.unwrap();
        if bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Current final")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Current final")
    );
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Stale final")
    );
}

#[tokio::test]
async fn background_reads_limit_parallel_exports_and_ignore_results_after_desk_selection() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let starts = root.path().join("starts");
    let release = root.path().join("release");
    let data = json!({"messages":[{"info":{"id":"new-final","role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":"Must stay muted"}]}]});
    std::fs::write(&config.opencode_bin, format!("#!/bin/sh\nprintf '%s\\n' \"$3\" >> {starts:?}\nwhile [ ! -e {release:?} ]; do sleep 0.01; done\nprintf '%s' '{data}'\n")).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let agents: Vec<_> = (0..6)
        .map(|index| {
            let mut a = agent();
            a.agent_session.as_mut().unwrap().value = format!("ses_{index}");
            a.pane_id = format!("w1:p{index}");
            bridge.state.mappings.insert(
                a.key(),
                Mapping {
                    group_id_hex: Some(format!("{index:032x}")),
                    output_initialised: true,
                    active: true,
                    agent_record: Some(a.clone()),
                    last_agent_status: "idle".into(),
                    last_state_change_seq: 1,
                    ..Mapping::default()
                },
            );
            a
        })
        .collect();
    bridge.poll_sessions(agents.clone()).await.unwrap();
    for _ in 0..1000 {
        if std::fs::read_to_string(&starts)
            .unwrap_or_default()
            .lines()
            .count()
            == 4
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read_to_string(&starts).unwrap().lines().count(), 4);
    assert!(forwarding_command(&config, "desk").await.status.success());
    std::fs::write(release, "").unwrap();
    for _ in 0..1000 {
        bridge.poll_sessions(agents.clone()).await.unwrap();
        if bridge
            .state
            .mappings
            .values()
            .all(|m| !m.forwarding_baseline_pending)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!bridge.state.forwarding_enabled);
    assert!(
        bridge
            .state
            .mappings
            .values()
            .all(|m| !m.forwarding_baseline_pending)
    );
    assert!(bridge.state.outbox.is_empty());
}

fn editable_config(dir: &std::path::Path) -> BridgeConfig {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    serde_json::from_value(json!({
        "phone_npub":phone_npub(), "herdr_socket":dir.join("herdr.sock"),
        "wn_agent_socket":dir.join("wn.sock"), "account_id_hex":"aa".repeat(32),
        "state_path":dir.join("state.json"), "opencode_bin":dir.join("opencode"),
        "codex_home":dir.join("codex"), "claude_home":dir.join("claude"),
        "edit_responses":true, "auto_forwarding":false
    }))
    .unwrap()
}

async fn forwarding_command(config: &BridgeConfig, mode: &str) -> std::process::Output {
    let path = config.state_path.with_file_name("test-config.toml");
    std::fs::write(&path,format!("phone_npub = {:?}\naccount_id_hex = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\nstate_path = {:?}\nopencode_bin = {:?}\ncodex_home = {:?}\nclaude_home = {:?}\nedit_responses = {}\n",config.phone_npub,config.account_id_hex,config.herdr_socket,config.wn_agent_socket,config.state_path,config.opencode_bin,config.codex_home,config.claude_home,config.edit_responses)).unwrap();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args(["--config", path.to_str().unwrap(), "forwarding", mode])
        .output()
        .await
        .unwrap()
}

fn permission_export(path: &std::path::Path) -> std::path::PathBuf {
    let request_path = path.with_file_name("permission.json");
    std::fs::write(
        &request_path,
        json!({"id":"per_test","sessionID":"ses_1","action":"shell",
        "resources":["git commit -m 'Test change'"],"save":["git commit *"],
        "source":{"type":"tool","messageID":"msg_permission","id":"tool_permission"},
        "metadata":{"command":"git commit -m 'Test change'"}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(path, r#"#!/usr/bin/env python3
import json, pathlib, sys
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
if args[0]=='--pure': sys.exit(2)
if args[0]=='session':
 print(json.dumps({'messages':[{'type':'user','id':'user','text':'Task'}]}));sys.exit(0)
if args[:2]==['api','get']:
 if args[2].endswith('/form'): print('{"data":[]}');sys.exit(0)
 request=json.loads((root/'permission.json').read_text())
 if args[2]=='/api/session/ses_1/permission': print(json.dumps({'data':[] if request is None else [request]}));sys.exit(0)
 if request and args[2]=='/api/session/ses_1/permission/'+request['id']: print(json.dumps({'data':request}));sys.exit(0)
 sys.exit(1)
if args[:2]==['api','post']:
 body=json.loads(args[args.index('--data')+1])
 with (root/'permission-posts.jsonl').open('a') as f: f.write(json.dumps({'path':args[2],'body':body})+'\n')
 if (root/'drop-permission-ack').exists():sys.exit(1)
 sys.exit(0)
sys.exit(2)
"#).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    request_path
}

#[tokio::test]
async fn permission_card_uses_request_bound_once_or_reject_without_keys_prompts_or_unmuting() {
    for (verb, expected) in [("/allow", "once"), ("/reject", "reject")] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        permission_export(&config.opencode_bin);
        let mut blocked = agent();
        blocked.agent_status = "blocked".into();
        blocked.state_change_seq = 2;
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr =
            peer_for_agent(&config.herdr_socket, false, calls.clone(), blocked.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.state.mappings.insert(
            blocked.key(),
            activity_mapping(blocked.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![blocked.clone()]).await.unwrap();
        let card = bridge
            .state
            .outbox
            .iter()
            .find(|item| item.text.starts_with("Permission ["))
            .expect("The blocked permission must produce a phone approval card")
            .text
            .clone();
        assert!(card.contains("git commit -m 'Test change'"));
        assert!(!card.contains("Always allow"));
        let code = card
            .lines()
            .find_map(|line| line.strip_prefix("/allow "))
            .unwrap();
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        let mut event = inbound(HEX, "permission-reply");
        event["message"]["text"] = json!(format!("{verb} {code}"));
        bridge.inbound(&event).await.unwrap();
        assert!(!bridge.state.forwarding_enabled);
        assert!(
            bridge.state.mappings[&blocked.key()]
                .prompt_queue
                .is_empty()
        );
        let posts: Vec<Value> = std::fs::read_to_string(root.path().join("permission-posts.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0]["path"],
            "/api/session/ses_1/permission/per_test/reply"
        );
        assert_eq!(posts[0]["body"], json!({"decision":expected}));
        assert!(!calls.lock().unwrap().iter().any(|call| call["method"] == "agent.send_keys" || call["method"] == "agent.prompt"));
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![blocked]).await.unwrap();
        event["message"]["message_id_hex"] = json!("another-reply-to-same-permission");
        bridge.inbound(&event).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("permission-posts.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        herdr.abort();
    }
}

#[tokio::test]
async fn permission_replies_fail_closed_for_stale_foreign_malformed_and_uncertain_requests() {
    for scenario in [
        "changed",
        "gone",
        "foreign",
        "wrong-code",
        "persistent",
        "numeric",
        "attachment",
        "unknown-result",
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let request_path = permission_export(&config.opencode_bin);
        let mut blocked = agent();
        blocked.agent_status = "blocked".into();
        blocked.state_change_seq = 2;
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr =
            peer_for_agent(&config.herdr_socket, false, calls.clone(), blocked.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.state.mappings.insert(
            blocked.key(),
            activity_mapping(blocked.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![blocked.clone()]).await.unwrap();
        let card = bridge
            .state
            .outbox
            .iter()
            .find(|item| item.text.starts_with("Permission ["))
            .unwrap()
            .text
            .clone();
        let code = card
            .lines()
            .find_map(|line| line.strip_prefix("/allow "))
            .unwrap();
        let mut event = inbound(HEX, "permission-fail-closed");
        event["message"]["text"] = json!(format!("/allow {code}"));
        match scenario {
            "changed" => {
                let mut request: Value =
                    serde_json::from_slice(&std::fs::read(&request_path).unwrap()).unwrap();
                request["metadata"]["command"] = json!("different command");
                std::fs::write(&request_path, request.to_string()).unwrap();
            }
            "gone" => std::fs::write(&request_path, "null").unwrap(),
            "foreign" => event["message"]["sender"]["account_id_hex"] = json!("cc".repeat(32)),
            "wrong-code" => event["message"]["text"] = json!("/allow pwrong"),
            "persistent" => event["message"]["text"] = json!(format!("/allow-always {code}")),
            "numeric" => event["message"]["text"] = json!("2"),
            "attachment" => event["message"]["attachments"] = json!([{"name":"input.txt"}]),
            "unknown-result" => {
                std::fs::write(root.path().join("drop-permission-ack"), "").unwrap()
            }
            _ => unreachable!(),
        }
        bridge.inbound(&event).await.unwrap();
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![blocked.clone()]).await.unwrap();
        event["message"]["message_id_hex"] = json!("permission-second-attempt");
        bridge.inbound(&event).await.unwrap();
        let posts =
            std::fs::read_to_string(root.path().join("permission-posts.jsonl")).unwrap_or_default();
        assert_eq!(
            posts.lines().count(),
            usize::from(scenario == "unknown-result"),
            "{scenario}"
        );
        assert!(
            bridge.state.mappings[&blocked.key()]
                .prompt_queue
                .is_empty(),
            "{scenario}"
        );
        assert!(!calls.lock().unwrap().iter().any(|call| call["method"] == "agent.send_keys" || call["method"] == "agent.prompt"), "{scenario}");
        herdr.abort();
    }
}

fn decision_export(path: &std::path::Path, id: &str, question: &str) {
    let data = json!({"messages":[
        {"info":{"id":"user-decision","role":"user"},"parts":[]},
        {"info":{"id":"assistant-decision","parentID":"user-decision","role":"assistant"},"parts":[
            {"type":"text","text":"PRIVATE REASONING"},
            {"type":"tool","id":id,"tool":"question","state":{"status":"running","input":{"questions":[{
                "header":"Next step","question":question,"options":[
                    {"label":"Proceed","description":"Continue with the plan"},
                    {"label":"Review","description":"Review the alternatives"}
                ]
            }]}}}
        ]}
    ]});
    std::fs::write(path, format!("#!/bin/sh\nprintf '%s' '{}'\n", data)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn decision_terminal(question: &str) -> String {
    format!(
        "PRIVATE TOOL OUTPUT\n{question}\n1. Proceed\n   Continue with the plan\n2. Review\n   Review the alternatives\n3. Type your own answer\n↑↓ select enter submit esc dismiss\n"
    )
}

fn custom_decision_export(path: &std::path::Path) -> std::path::PathBuf {
    let form = path.with_file_name("question-form.json");
    std::fs::write(&form, json!({"id":"frm_amber","sessionID":"ses_1","metadata":{"kind":"question","tool":{"messageID":"msg_amber","id":"tool_amber"}},
        "fields":[{"key":"q0","type":"string","title":"Amber version","description":"Which Amber version is installed?","custom":true,
        "options":[{"value":"I can check","label":"I can check"},{"value":"Not sure","label":"Not sure"}]}]}).to_string()).unwrap();
    std::fs::write(path, r#"#!/usr/bin/env python3
import json,pathlib,sys
root=pathlib.Path(__file__).parent;args=sys.argv[1:]
if args[0]=='--pure':sys.exit(2)
if args[0]=='session':print('{"messages":[{"type":"user","id":"user","text":"Task"}]}');sys.exit(0)
if args[:2]==['api','get']:
 if args[2].endswith('/permission'):print('{"data":[]}');sys.exit(0)
 if args[2].endswith('/form'):print(json.dumps({'data':[json.loads((root/'question-form.json').read_text())]}));sys.exit(0)
 sys.exit(2)
if args[:2]==['api','post']:
 body=json.loads(args[args.index('--data')+1])
 state=json.loads((root/'state.json').read_text())
 assert any((m.get('decision') or {}).get('submitted') for m in state['mappings'].values())
 with (root/'question-posts.jsonl').open('a') as f:f.write(json.dumps({'path':args[2],'body':body})+'\n')
 if (root/'drop-question-ack').exists():sys.exit(1)
 sys.exit(0)
sys.exit(2)
"#).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    form
}

#[tokio::test]
async fn custom_question_text_uses_the_request_bound_form_api_once_without_prompting_or_keys() {
    for lost_ack in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = editable_config(root.path());
        custom_decision_export(&config.opencode_bin);
        if lost_ack {
            std::fs::write(root.path().join("drop-question-ack"), "").unwrap();
        }
        let mut blocked = agent();
        blocked.agent_status = "blocked".into();
        blocked.state_change_seq = 2;
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = decision_herdr(
            &config.herdr_socket,
            Arc::new(Mutex::new(blocked.clone())),
            Arc::new(Mutex::new(
                "No keyboard input is needed for native form replies".into(),
            )),
            calls.clone(),
        )
        .await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let key = blocked.key();
        bridge.state.mappings.insert(
            key.clone(),
            activity_mapping(blocked.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![blocked.clone()]).await.unwrap();
        let mut event = inbound(HEX, "custom-version");
        event["message"]["text"] = json!("v6.6.6");
        bridge.inbound(&event).await.unwrap();
        bridge.inbound(&event).await.unwrap();
        assert!(
            bridge.state.mappings[&key]
                .decision
                .as_ref()
                .unwrap()
                .submitted
        );
        bridge.save().unwrap();
        drop(bridge);
        let mut bridge = Bridge::open(config).unwrap();
        bridge.agents.insert(key.clone(), blocked);
        bridge.herdr_available = true;
        bridge.inbound(&event).await.unwrap();
        event["message"]["message_id_hex"] = json!("custom-again");
        bridge.inbound(&event).await.unwrap();
        let posts: Vec<Value> = std::fs::read_to_string(root.path().join("question-posts.jsonl"))
            .expect("Custom answers must use the native form endpoint")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["path"], "/api/session/ses_1/form/frm_amber/reply");
        assert_eq!(posts[0]["body"], json!({"answer":{"q0":"v6.6.6"}}));
        assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
        assert!(!calls.lock().unwrap().iter().any(|r| matches!(
            r["method"].as_str(),
            Some("agent.prompt" | "agent.send_keys" | "agent.read")
        )));
        herdr.abort();
    }
}

#[tokio::test]
async fn custom_question_answers_fail_closed_for_changed_requests_foreign_input_and_failed_saves() {
    for choice in [false, true] {
        for scenario in [
            "custom-disabled",
            "changed-form",
            "changed-options",
            "multiple",
            "no-longer-blocked",
            "foreign",
            "changed-session",
            "stale-code",
            "truncated",
            "attachment",
            "prepared-file",
            "failed-save",
        ] {
            if choice && matches!(scenario, "custom-disabled" | "prepared-file") {
                // Both cases restrict custom text, not existing choice routing.
                continue;
            }
            let root = tempfile::tempdir().unwrap();
            let config = editable_config(root.path());
            let form_path = custom_decision_export(&config.opencode_bin);
            let mut blocked = agent();
            blocked.agent_status = "blocked".into();
            blocked.state_change_seq = 2;
            let current = Arc::new(Mutex::new(blocked.clone()));
            let calls = Arc::new(Mutex::new(vec![]));
            let herdr = decision_herdr(
                &config.herdr_socket,
                current.clone(),
                Arc::new(Mutex::new(String::new())),
                calls.clone(),
            )
            .await;
            let mut bridge = Bridge::open(config.clone()).unwrap();
            let key = blocked.key();
            bridge.state.mappings.insert(
                key.clone(),
                activity_mapping(blocked.clone(), &"bb".repeat(16)),
            );
            if scenario == "custom-disabled" {
                let mut form: Value =
                    serde_json::from_slice(&std::fs::read(&form_path).unwrap()).unwrap();
                form["fields"][0]["custom"] = json!(false);
                std::fs::write(&form_path, form.to_string()).unwrap();
            }
            bridge.reconcile(vec![blocked]).await.unwrap();
            let mut event = inbound(HEX, "unsafe-custom");
            let answer = if choice { "1" } else { "v6.6.6" };
            event["message"]["text"] = json!(answer);
            match scenario {
                "changed-form" => {
                    let mut form: Value =
                        serde_json::from_slice(&std::fs::read(&form_path).unwrap()).unwrap();
                    form["id"] = json!("frm_replaced");
                    std::fs::write(&form_path, form.to_string()).unwrap();
                }
                "changed-options" | "multiple" => {
                    let mut form: Value =
                        serde_json::from_slice(&std::fs::read(&form_path).unwrap()).unwrap();
                    if scenario == "multiple" {
                        form["fields"][0]["type"] = json!("multiselect");
                    } else {
                        form["fields"][0]["options"][0] =
                            json!({"value":"Different option","label":"Different option"});
                    }
                    std::fs::write(&form_path, form.to_string()).unwrap();
                }
                "no-longer-blocked" => current.lock().unwrap().agent_status = "working".into(),
                "foreign" => event["message"]["sender"]["account_id_hex"] = json!("22".repeat(32)),
                "changed-session" => {
                    current
                        .lock()
                        .unwrap()
                        .agent_session
                        .as_mut()
                        .unwrap()
                        .value = "ses_replaced".into()
                }
                "stale-code" => event["message"]["text"] = json!(format!("qold001 {answer}")),
                "truncated" => event["message"]["text_truncated"] = json!(true),
                "attachment" => {
                    event["message"]["attachments"] = json!([{"file_name":"version.txt"}])
                }
                "failed-save" => {
                    std::fs::remove_file(&config.state_path).unwrap();
                    std::fs::create_dir(&config.state_path).unwrap();
                }
                _ => {}
            }
            if scenario == "prepared-file" {
                event["_codr_prepared_text"] = json!(answer);
                bridge.state.media_pending.push(event);
                let _ = bridge.process_media().await;
                assert_eq!(
                    bridge.state.media_pending.len(),
                    1,
                    "Prepared media is not a custom decision answer"
                );
            } else {
                let _ = bridge.inbound(&event).await;
            }
            assert!(
                !root.path().join("question-posts.jsonl").exists(),
                "Unsafe native form submission: {scenario}"
            );
            if scenario == "failed-save" {
                assert!(
                    !bridge.state.mappings[&key]
                        .decision
                        .as_ref()
                        .unwrap()
                        .submitted
                );
            }
            assert!(!calls.lock().unwrap().iter().any(|r| matches!(
                r["method"].as_str(),
                Some("agent.prompt" | "agent.send_keys")
            )));
            herdr.abort();
        }
    }
}

#[tokio::test]
async fn native_question_choices_use_the_bound_form_without_terminal_keys() {
    for (answer, expected, custom_allowed, lost_ack) in [
        ("1", "I can check", true, false),
        ("I can check", "I can check", true, false),
        ("2", "Not sure", false, false),
        ("Not sure", "Not sure", false, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = editable_config(root.path());
        let form_path = custom_decision_export(&config.opencode_bin);
        let mut form: Value = serde_json::from_slice(&std::fs::read(&form_path).unwrap()).unwrap();
        form["fields"][0]["custom"] = json!(custom_allowed);
        std::fs::write(form_path, form.to_string()).unwrap();
        if lost_ack {
            std::fs::write(root.path().join("drop-question-ack"), "").unwrap();
        }
        let mut blocked = agent();
        blocked.agent_status = "blocked".into();
        blocked.state_change_seq = 2;
        let calls = Arc::new(Mutex::new(vec![]));
        let herdr = decision_herdr(
            &config.herdr_socket,
            Arc::new(Mutex::new(blocked.clone())),
            Arc::new(Mutex::new("A differently rendered native question".into())),
            calls.clone(),
        )
        .await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let key = blocked.key();
        bridge.state.mappings.insert(
            key.clone(),
            activity_mapping(blocked.clone(), &"bb".repeat(16)),
        );
        bridge.reconcile(vec![blocked.clone()]).await.unwrap();
        let mut event = inbound(HEX, "native-choice");
        event["message"]["text"] = json!(answer);
        bridge.inbound(&event).await.unwrap();
        assert!(
            bridge.state.mappings[&key]
                .decision
                .as_ref()
                .unwrap()
                .submitted
        );
        bridge.inbound(&event).await.unwrap();
        drop(bridge);
        let mut bridge = Bridge::open(config).unwrap();
        bridge.agents.insert(key.clone(), blocked);
        bridge.herdr_available = true;
        event["message"]["message_id_hex"] = json!("same-choice-again");
        bridge.inbound(&event).await.unwrap();
        let posts: Vec<Value> = std::fs::read_to_string(root.path().join("question-posts.jsonl"))
            .expect("Native choices must use the form API")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["path"], "/api/session/ses_1/form/frm_amber/reply");
        assert_eq!(posts[0]["body"], json!({"answer":{"q0":expected}}));
        assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
        assert!(!calls.lock().unwrap().iter().any(|r| matches!(
            r["method"].as_str(),
            Some("agent.prompt" | "agent.send_keys" | "agent.read")
        )));
        herdr.abort();
    }
}

#[tokio::test]
async fn an_old_custom_question_code_cannot_answer_an_already_announced_replacement_form() {
    let root = tempfile::tempdir().unwrap();
    let config = editable_config(root.path());
    let form_path = custom_decision_export(&config.opencode_bin);
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(
        &config.herdr_socket,
        Arc::new(Mutex::new(blocked.clone())),
        Arc::new(Mutex::new(String::new())),
        calls.clone(),
    )
    .await;
    let mut bridge = Bridge::open(config).unwrap();
    let key = blocked.key();
    bridge.state.mappings.insert(
        key.clone(),
        activity_mapping(blocked.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![blocked]).await.unwrap();
    let old_code = bridge.state.mappings[&key]
        .decision
        .as_ref()
        .unwrap()
        .token();
    let mut form: Value = serde_json::from_slice(&std::fs::read(&form_path).unwrap()).unwrap();
    form["id"] = json!("frm_replaced");
    std::fs::write(&form_path, form.to_string()).unwrap();
    let mut event = inbound(HEX, "announce-replacement");
    event["message"]["text"] = json!("v6.6.6");
    bridge.inbound(&event).await.unwrap();
    assert!(
        bridge.state.mappings[&key]
            .decision
            .as_ref()
            .unwrap()
            .requires_token
    );
    event["message"]["message_id_hex"] = json!("old-coded-answer");
    event["message"]["text"] = json!(format!("{old_code} v6.6.6"));
    bridge.inbound(&event).await.unwrap();
    assert!(
        !root.path().join("question-posts.jsonl").exists(),
        "A delayed old coded answer must not reach the replacement form"
    );
    let new_code = bridge.state.mappings[&key]
        .decision
        .as_ref()
        .unwrap()
        .token();
    event["message"]["message_id_hex"] = json!("new-coded-answer");
    event["message"]["text"] = json!(format!("{new_code} v6.6.6"));
    bridge.inbound(&event).await.unwrap();
    let posts: Vec<Value> = std::fs::read_to_string(root.path().join("question-posts.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0]["path"],
        "/api/session/ses_1/form/frm_replaced/reply"
    );
    herdr.abort();
}

#[tokio::test]
async fn coded_decision_with_repository_footer_answers_once_without_clearing_the_queue() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    decision_export(&config.opencode_bin, "question-1", "Which next step?");
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let mut visible = decision_terminal("Which next step?")
        .lines()
        .map(|line| format!("{:<80}  sidebar", format!("  ┃ {line}")))
        .collect::<Vec<_>>();
    visible.push(format!("{:<80}  ~/code/example:main", "  ┃"));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(
        &config.herdr_socket,
        Arc::new(Mutex::new(blocked.clone())),
        Arc::new(Mutex::new(visible.join("\n"))),
        calls.clone(),
    )
    .await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"bb".repeat(16)));
    bridge.reconcile(vec![blocked.clone()]).await.unwrap();
    accept_legacy_text(
        &mut bridge,
        &agent().key(),
        "waiting-correction",
        "Accepted before upgrade",
    );
    let decision = bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .decision
        .as_mut()
        .unwrap();
    decision.requires_token = true;
    let mut answer = inbound(HEX, "coded-choice");
    answer["message"]["text"] = json!(format!("{} 1", decision.token()));
    bridge.inbound(&answer).await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![blocked]).await.unwrap();
    bridge.inbound(&answer).await.unwrap();
    assert_eq!(bridge.state.mappings[&agent().key()].prompt_queue.len(), 1);
    assert!(
        bridge.state.mappings[&agent().key()]
            .decision
            .as_ref()
            .unwrap()
            .submitted
    );
    let calls = calls.lock().unwrap();
    let choices: Vec<_> = calls
        .iter()
        .filter(|call| call["method"] == "agent.send_keys")
        .collect();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0]["params"]["keys"], json!(["1"]));
    assert!(!calls.iter().any(|call| call["method"] == "agent.prompt"));
    herdr.abort();
}

#[tokio::test]
async fn a_permission_card_does_not_intercept_a_later_numeric_question_answer_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    permission_export(&config.opencode_bin);
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(
        &config.herdr_socket,
        observed.clone(),
        Arc::new(Mutex::new(decision_terminal("Which next step?"))),
        calls.clone(),
    )
    .await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        blocked.key(),
        activity_mapping(blocked.clone(), &"bb".repeat(16)),
    );
    bridge.reconcile(vec![blocked.clone()]).await.unwrap();
    assert!(bridge.state.mappings[&blocked.key()].permission.is_some());
    drop(bridge);
    decision_export(
        &config.opencode_bin,
        "question-after-permission",
        "Which next step?",
    );
    blocked.state_change_seq += 1;
    *observed.lock().unwrap() = blocked.clone();
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![blocked.clone()]).await.unwrap();
    assert!(bridge.state.mappings[&blocked.key()].permission.is_none());
    let mut answer = inbound(HEX, "later-numeric-question");
    answer["message"]["text"] = json!("1");
    bridge.inbound(&answer).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "agent.send_keys")
            .count(),
        1
    );
    herdr.abort();
}

async fn decision_herdr(
    path: &std::path::Path,
    observed: Arc<Mutex<Agent>>,
    visible: Arc<Mutex<String>>,
    calls: Arc<Mutex<Vec<Value>>>,
) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            calls.lock().unwrap().push(request.clone());
            let mut response = match request["method"].as_str().unwrap() {
                "agent.get" => json!({"result":{"agent":*observed.lock().unwrap()}}),
                "agent.read" => {
                    json!({"result":{"read":{"text":*visible.lock().unwrap(),"truncated":false}}})
                }
                "agent.send_keys" => {
                    if visible.lock().unwrap().starts_with("DROP_SELECTION_ACK") {
                        continue;
                    }
                    assert_eq!(
                        request["params"]["keys"].as_array().unwrap().len(),
                        1,
                        "Never append Enter after a numbered single-choice shortcut"
                    );
                    let mut current = observed.lock().unwrap();
                    current.agent_status = "working".into();
                    current.state_change_seq += 1;
                    json!({"result":{"type":"ok"}})
                }
                _ => panic!("Decision replies must not use agent.prompt"),
            };
            response["id"] = request["id"].clone();
            reader
                .get_mut()
                .write_all((response.to_string() + "\n").as_bytes())
                .await
                .unwrap();
        }
    })
}

#[tokio::test]
async fn a_phone_choice_answers_the_blocked_question_instead_of_becoming_a_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    decision_export(&config.opencode_bin, "question-1", "Which next step?");
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let visible = Arc::new(Mutex::new(decision_terminal("Which next step?")));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(
        &config.herdr_socket,
        observed.clone(),
        visible,
        calls.clone(),
    )
    .await;
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            pending_prompt: true,
            agent_record: Some(agent()),
            last_agent_status: "working".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![blocked]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| { call["text"] == "> blocked Waiting for your decision." })
    );
    let mut answer = inbound(HEX, "choice-1");
    let mut next_task = inbound(HEX, "task-during-decision");
    next_task["message"]["text"] = json!("Do the next task after this decision");
    bridge.inbound(&next_task).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&agent().key()].prompt_queue.len(),
        0,
        "A blocked native agent cannot accept ordinary text; do not silently create a future task"
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("No prompt was queued"))
    );
    answer["message"]["text"] = json!("1");
    bridge.inbound(&answer).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&agent().key()].prompt_queue.len(),
        0,
        "The supported decision answer bypasses the queue"
    );
    bridge.flush().await.unwrap();
    let selected: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "agent.send_keys")
        .cloned()
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "A blocked question is waiting for an answer, not a new prompt"
    );
    assert_eq!(selected[0]["params"]["keys"], json!(["1"]));
    assert!(
        !calls.lock().unwrap().iter().any(|r| r["text"]
            .as_str()
            .is_some_and(|text| text.contains("PRIVATE TOOL OUTPUT")
                || text.contains("PRIVATE REASONING")
                || text.contains("busy or blocked")))
    );
    let mut restarted = Bridge::open(config).unwrap();
    let current = observed.lock().unwrap().clone();
    restarted.reconcile(vec![current]).await.unwrap();
    restarted.inbound(&answer).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "agent.send_keys")
            .count(),
        1
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn desk_mode_discards_updates_and_away_does_not_replay_them() {
    for edits in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.edit_responses = edits;
        export_script(&config.opencode_bin, "Old reply", "old");
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![agent()]).await.unwrap();
        bridge.flush().await.unwrap();
        calls.lock().unwrap().clear();
        let result = forwarding_command(&config, "desk").await;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        export_script(&config.opencode_bin, "Muted reply", "muted");
        let mut done = agent();
        done.state_change_seq = 2;
        bridge.reconcile(vec![done.clone()]).await.unwrap();
        bridge.flush().await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        assert!(
            bridge.state.mappings[&agent().key()]
                .final_ids
                .contains(&"muted".into())
        );
        assert!(bridge.state.outbox.is_empty());
        assert!(forwarding_command(&config, "away").await.status.success());
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![done]).await.unwrap();
        bridge.flush().await.unwrap();
        assert!(calls.lock().unwrap().is_empty(), "No backlog on resume");
        let mut working = agent();
        working.agent_status = "working".into();
        working.state_change_seq = 3;
        bridge.reconcile(vec![working]).await.unwrap();
        bridge.flush().await.unwrap();
        export_script(&config.opencode_bin, "Future reply", "future");
        let mut done = agent();
        done.state_change_seq = 4;
        bridge.reconcile(vec![done]).await.unwrap();
        bridge.flush().await.unwrap();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request["text"] == "Future reply")
        );
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request["text"] == "Muted reply")
        );
        mdk.abort();
    }
}

#[tokio::test]
async fn only_a_new_authorised_phone_message_wakes_forwarding_for_all_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "Old reply", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut other = agent();
    other.agent_session.as_mut().unwrap().value = "ses_2".into();
    other.pane_id = "w1:p2".into();
    other.terminal_id = "term2".into();
    for (observed, group) in [(agent(), "bb".repeat(16)), (other.clone(), "dd".repeat(16))] {
        bridge.state.mappings.insert(
            observed.key(),
            Mapping {
                group_id_hex: Some(group),
                active: true,
                agent_record: Some(observed),
                last_agent_status: "idle".into(),
                last_state_change_seq: 1,
                final_ids: vec!["old".into()],
                output_initialised: true,
                ..Mapping::default()
            },
        );
    }
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .seen_messages
        .push("already".into());
    bridge.save().unwrap();
    assert!(forwarding_command(&config, "desk").await.status.success());
    bridge
        .reconcile(vec![agent(), other.clone()])
        .await
        .unwrap();
    bridge.flush().await.unwrap();
    bridge
        .inbound(&inbound(&"ff".repeat(32), "foreign"))
        .await
        .unwrap();
    let mut orphan = inbound(HEX, "orphan");
    orphan["group_id_hex"] = json!("ee".repeat(16));
    bridge.inbound(&orphan).await.unwrap();
    bridge.inbound(&inbound(HEX, "already")).await.unwrap();
    assert_eq!(
        serde_json::to_value(&bridge.state).unwrap()["forwarding_enabled"],
        false
    );
    let mut wake = inbound(HEX, "wake");
    wake["message"]["text"] = json!("/status");
    bridge.inbound(&wake).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        calls.lock().unwrap().iter().any(|call| {
            call["text"] == "> idle herdr session: connected. 0 waiting prompt(s)."
        })
    );
    assert_eq!(
        serde_json::to_value(&bridge.state).unwrap()["forwarding_enabled"],
        true
    );
    let mut restarted = Bridge::open(config.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&restarted.state).unwrap()["forwarding_enabled"],
        true
    );
    other.agent_status = "working".into();
    other.state_change_seq = 2;
    restarted.reconcile(vec![agent(), other]).await.unwrap();
    restarted.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["group_id_hex"] == "dd".repeat(16)
                && request["text"] == "> working...")
    );
    assert!(forwarding_command(&config, "desk").await.status.success());
    restarted.reconcile(vec![agent()]).await.unwrap();
    restarted.inbound(&wake).await.unwrap();
    assert_eq!(
        serde_json::to_value(&restarted.state).unwrap()["forwarding_enabled"],
        false,
        "History replay must not undo a later Desk choice"
    );
    mdk.abort();
}

#[tokio::test]
async fn a_replaced_question_does_not_accept_an_old_unqualified_answer() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    decision_export(&config.opencode_bin, "question-a", "First question?");
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let visible = Arc::new(Mutex::new(decision_terminal("First question?")));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(
        &config.herdr_socket,
        observed,
        visible.clone(),
        calls.clone(),
    )
    .await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            pending_prompt: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![blocked]).await.unwrap();
    decision_export(&config.opencode_bin, "question-b", "Second question?");
    *visible.lock().unwrap() = decision_terminal("Second question?");
    let mut answer = inbound(HEX, "old-answer");
    answer["message"]["text"] = json!("1");
    bridge.inbound(&answer).await.unwrap();
    answer["message"]["message_id_hex"] = json!("still-old-answer");
    bridge.inbound(&answer).await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "agent.send_keys")
    );
    let decision = bridge.state.mappings[&agent().key()]
        .decision
        .as_ref()
        .unwrap();
    assert!(decision.requires_token);
    answer["message"]["message_id_hex"] = json!("current-answer");
    answer["message"]["text"] = json!(format!("{} 2", decision.token()));
    bridge.inbound(&answer).await.unwrap();
    let selected: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "agent.send_keys")
        .cloned()
        .collect();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["params"]["keys"], json!(["2"]));
    herdr.abort();
}

#[tokio::test]
async fn permission_or_changed_terminal_ui_cannot_receive_a_question_shortcut() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    decision_export(&config.opencode_bin, "question-1", "Which next step?");
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let visible = Arc::new(Mutex::new(
        "Allow writing files? enter confirm esc cancel".into(),
    ));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(&config.herdr_socket, observed, visible, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            pending_prompt: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![blocked]).await.unwrap();
    let mut answer = inbound(HEX, "invalid-ui");
    answer["message"]["text"] = json!("1");
    bridge.inbound(&answer).await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "agent.send_keys")
    );
    assert!(
        !bridge.state.mappings[&agent().key()]
            .decision
            .as_ref()
            .unwrap()
            .submitted
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|message| message.text.contains("No answer was sent"))
    );
    herdr.abort();
}

#[tokio::test]
async fn an_uncertain_decision_shortcut_is_not_sent_again_on_retry_or_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    decision_export(&config.opencode_bin, "question-1", "Which next step?");
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let visible = Arc::new(Mutex::new(format!(
        "DROP_SELECTION_ACK\n{}",
        decision_terminal("Which next step?")
    )));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(&config.herdr_socket, observed, visible, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            pending_prompt: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![blocked.clone()]).await.unwrap();
    let mut answer = inbound(HEX, "uncertain-answer");
    answer["message"]["text"] = json!("1");
    bridge.inbound(&answer).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .decision
            .as_ref()
            .unwrap()
            .submitted
    );
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![blocked]).await.unwrap();
    restarted.inbound(&answer).await.unwrap();
    answer["message"]["message_id_hex"] = json!("another-answer");
    restarted.inbound(&answer).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "agent.send_keys")
            .count(),
        1
    );
    herdr.abort();
}

#[tokio::test]
async fn a_whitespace_or_uppercase_coded_answer_cannot_become_a_new_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    decision_export(&config.opencode_bin, "question-1", "Which next step?");
    let mut blocked = agent();
    blocked.agent_status = "blocked".into();
    blocked.state_change_seq = 2;
    let observed = Arc::new(Mutex::new(blocked.clone()));
    let visible = Arc::new(Mutex::new(decision_terminal("Which next step?")));
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = decision_herdr(
        &config.herdr_socket,
        observed.clone(),
        visible,
        calls.clone(),
    )
    .await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![blocked]).await.unwrap();
    let token = bridge.state.mappings[&agent().key()]
        .decision
        .as_ref()
        .unwrap()
        .token();
    let mut idle = agent();
    idle.state_change_seq = 3;
    *observed.lock().unwrap() = idle.clone();
    export_script(&config.opencode_bin, "Finished", "finished");
    bridge.reconcile(vec![idle]).await.unwrap();
    let mut answer = inbound(HEX, "stale-coded");
    answer["message"]["text"] = json!(format!(" {} 1", token.to_uppercase()));
    bridge.inbound(&answer).await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "agent.prompt")
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|message| message.text.contains("no longer waiting"))
    );
    herdr.abort();
}

#[tokio::test]
async fn forwarding_controls_work_while_the_worker_holds_its_state_lock() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let bridge = Bridge::open(config.clone()).unwrap();
    let _worker = herdr_white_noise_bridge::state::lock(&config.state_path).unwrap();
    let original = std::fs::read(&config.state_path).unwrap();
    let desk = forwarding_command(&config, "desk").await;
    assert!(
        desk.status.success(),
        "{}",
        String::from_utf8_lossy(&desk.stderr)
    );
    assert!(String::from_utf8_lossy(&desk.stdout).contains("At desk"));
    assert!(forwarding_command(&config, "status").await.status.success());
    let away = forwarding_command(&config, "toggle").await;
    assert!(away.status.success());
    assert!(String::from_utf8_lossy(&away.stdout).contains("Away"));
    let control = config.state_path.with_extension("forwarding.json");
    assert_eq!(
        std::fs::metadata(&control).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let current = std::fs::read(&control).unwrap();
    assert!(forwarding_command(&config, "away").await.status.success());
    assert_eq!(
        std::fs::read(&control).unwrap(),
        current,
        "Setting the current mode must not create another reset"
    );
    assert_eq!(
        std::fs::read(&config.state_path).unwrap(),
        original,
        "Only the worker writes routing state"
    );
    assert_eq!(bridge.state.mappings.len(), 0);
}

#[tokio::test]
async fn a_deferred_phone_wake_does_not_undo_a_later_desk_selection() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    assert!(forwarding_command(&config, "desk").await.status.success());
    let mut wake = inbound(HEX, "deferred");
    wake["message"]["text"] = json!("/status");
    assert!(bridge.inbound(&wake).await.is_err());
    assert!(
        String::from_utf8_lossy(&forwarding_command(&config, "status").await.stdout)
            .contains("Away")
    );
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .is_empty(),
        "The prompt must still be recoverable"
    );
    assert!(forwarding_command(&config, "desk").await.status.success());
    assert!(bridge.inbound(&wake).await.is_err());
    assert!(
        String::from_utf8_lossy(&forwarding_command(&config, "status").await.stdout)
            .contains("At desk"),
        "Retrying one received message must not apply its wake twice"
    );
}

#[tokio::test]
async fn selecting_desk_during_a_transcript_read_still_allows_an_invitation() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");
    std::fs::write(&config.opencode_bin,format!("#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '%s' '{{\"messages\":[]}}'\n",ready.display(),release.display())).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let reconcile = tokio::spawn(async move {
        bridge.reconcile(vec![agent()]).await.unwrap();
        bridge
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !ready.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(forwarding_command(&config, "desk").await.status.success());
    std::fs::write(release, "").unwrap();
    let bridge = reconcile.await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "group_create_with_admins")
    );
    assert!(bridge.state.mappings[&agent().key()].group_id_hex.is_some());
    assert!(!bridge.state.forwarding_enabled);
    assert!(bridge.state.outbox.is_empty());
    mdk.abort();
}

#[tokio::test]
async fn a_late_record_from_a_muted_turn_is_not_forwarded_after_resume() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    let write = |final_reply: Option<(&str, &str)>, turn: &str| {
        let mut messages = vec![json!({"info":{"id":turn,"role":"user"},"parts":[]})];
        if let Some((id, text)) = final_reply {
            messages.push(json!({"info":{"id":id,"parentID":turn,"role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":text}]}));
        }
        std::fs::write(
            &config.opencode_bin,
            format!(
                "#!/bin/sh\nprintf '%s' '{}'\n",
                json!({"messages":messages})
            ),
        )
        .unwrap();
        std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700))
            .unwrap();
    };
    write(None, "muted-user-turn");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(agent()),
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    assert!(forwarding_command(&config, "desk").await.status.success());
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(forwarding_command(&config, "away").await.status.success());
    bridge.reconcile(vec![agent()]).await.unwrap();
    write(Some(("late", "Late muted reply")), "muted-user-turn");
    let mut idle = agent();
    idle.state_change_seq = 2;
    bridge.reconcile(vec![idle.clone()]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["text"] == "Late muted reply")
    );
    write(Some(("new", "New away reply")), "new-user-turn");
    idle.state_change_seq = 3;
    bridge.reconcile(vec![idle]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["text"] == "New away reply")
    );
    mdk.abort();
}

#[tokio::test]
async fn global_phone_wake_checkpoints_other_sessions_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "Old reply", "old");
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut other = agent();
    other.agent_session.as_mut().unwrap().value = "ses_other".into();
    for (observed, group) in [(agent(), "bb".repeat(16)), (other.clone(), "dd".repeat(16))] {
        bridge.state.mappings.insert(
            observed.key(),
            Mapping {
                group_id_hex: Some(group),
                active: true,
                agent_record: Some(observed),
                last_agent_status: "idle".into(),
                last_state_change_seq: 1,
                final_ids: vec!["old".into()],
                ..Mapping::default()
            },
        );
    }
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    assert!(forwarding_command(&config, "desk").await.status.success());
    bridge
        .reconcile(vec![agent(), other.clone()])
        .await
        .unwrap();
    let mut wake = inbound(HEX, "wake");
    wake["message"]["text"] = json!("/status");
    bridge.inbound(&wake).await.unwrap();
    export_script(
        &config.opencode_bin,
        "Reply completed after wake",
        "after-wake",
    );
    other.state_change_seq = 2;
    bridge.reconcile(vec![agent(), other]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["group_id_hex"] == "dd".repeat(16)
                && request["text"] == "Reply completed after wake")
    );
    mdk.abort();
}

#[tokio::test]
async fn a_database_upgrade_recovers_phone_input_once_without_replaying_muted_history() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    let path = root.path().join("opencode.db");
    config.opencode_db = Some(path.clone());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE session(id TEXT); INSERT INTO session VALUES('ses_1');
        CREATE TABLE event(id TEXT,aggregate_id TEXT,seq INTEGER,type TEXT,data TEXT);
        INSERT INTO event VALUES('event-0','ses_1',0,'session.created.1','{}');
        CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,data TEXT);
        CREATE TABLE part(id TEXT,session_id TEXT,message_id TEXT,data TEXT);",
    )
    .unwrap();
    let old = json!({"info":{"id":"ses_1"},"messages":[{"type":"user","id":"muted-user","text":"old prompt"},{"type":"assistant","id":"muted-reply","finish":"stop","time":{"completed":1},"content":[{"type":"text","text":"Never replay this reply"}]}]});
    let calls_path = root.path().join("cli-calls");
    let failing = root.path().join("export-fails");
    std::fs::write(&config.opencode_bin, format!("#!/bin/sh\nif [ \"$1\" = --pure ] || [ -f {failing:?} ]; then exit 2; fi\nprintf x >> {calls_path:?}\nprintf '%s' '{old}'\n")).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            output_initialised: true,
            active: true,
            agent_record: Some(agent()),
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(
        !calls_path.exists(),
        "Compatible v1 history must stay on SQLite"
    );
    db.execute("DELETE FROM event", []).unwrap();
    std::fs::write(&failing, b"fixture").unwrap();
    assert!(forwarding_command(&config, "desk").await.status.success());
    bridge.poll_sessions(vec![agent()]).await.unwrap();
    let event = inbound(HEX, "phone-after-upgrade");
    assert!(bridge.inbound(&event).await.is_err());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(bridge.poll_ready_sessions().await.is_err());
    assert!(
        !bridge.state.mappings[&agent().key()]
            .seen_messages
            .contains(&"phone-after-upgrade".into())
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "agent.prompt")
    );
    assert!(bridge.state.forwarding_resume_pending);
    std::fs::remove_file(&failing).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let _ = bridge.poll_ready_sessions().await;
            if bridge.inbound(&event).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(bridge.state.forwarding_enabled && !bridge.state.forwarding_resume_pending);
    assert!(calls_path.exists());
    assert!(
        bridge.state.mappings[&agent().key()]
            .final_ids
            .contains(&"muted-reply".into())
    );
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("Never replay"))
    );
    bridge.inbound(&event).await.unwrap();
    drop(bridge);
    let mut reopened = Bridge::open(config).unwrap();
    reopened.inbound(&event).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "agent.prompt")
            .count(),
        1
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn a_superseded_muted_turn_stays_muted_while_the_current_turn_can_reply() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    let user = |id: &str| json!({"info":{"id":id,"role":"user"},"parts":[]});
    let reply = |id: &str, parent: &str, text: &str| json!({"info":{"id":id,"parentID":parent,"role":"assistant","time":{"completed":1},"finish":"stop"},"parts":[{"type":"text","text":text}]});
    let write = |messages: Vec<Value>| {
        std::fs::write(
            &config.opencode_bin,
            format!(
                "#!/bin/sh\nprintf '%s' '{}'\n",
                json!({"messages":messages})
            ),
        )
        .unwrap();
        std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700))
            .unwrap();
    };
    write(vec![user("turn-a"), user("turn-b")]);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut working = agent();
    working.agent_status = "working".into();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(working.clone()),
            last_agent_status: "working".into(),
            ..Mapping::default()
        },
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    assert!(forwarding_command(&config, "desk").await.status.success());
    bridge.reconcile(vec![working.clone()]).await.unwrap();
    assert!(forwarding_command(&config, "away").await.status.success());
    bridge.reconcile(vec![working]).await.unwrap();
    bridge.flush().await.unwrap();
    write(vec![
        user("turn-a"),
        reply("late-a", "turn-a", "Late muted A"),
        user("turn-b"),
    ]);
    let mut idle = agent();
    idle.state_change_seq = 2;
    bridge.reconcile(vec![idle.clone()]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["text"] == "Late muted A")
    );
    write(vec![
        user("turn-a"),
        reply("late-a", "turn-a", "Late muted A"),
        user("turn-b"),
        reply("final-b", "turn-b", "Current away B"),
    ]);
    idle.state_change_seq = 3;
    bridge.reconcile(vec![idle]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["text"] == "Current away B")
    );
    mdk.abort();
}

#[tokio::test]
async fn legacy_state_reports_away_requested_until_the_worker_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut legacy = BridgeState::default();
    legacy.mappings.insert(agent().key(), Mapping::default());
    legacy.save(&config.state_path).unwrap();
    assert!(forwarding_command(&config, "desk").await.status.success());
    assert!(forwarding_command(&config, "away").await.status.success());
    let status = forwarding_command(&config, "status").await;
    assert!(String::from_utf8_lossy(&status.stdout).contains("Away requested"));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args([
            "--config",
            config
                .state_path
                .with_file_name("test-config.toml")
                .to_str()
                .unwrap(),
            "status",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let row: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(row["forwarding_enabled"], false);
    assert_eq!(row["forwarding_resume_pending"], true);
}

#[tokio::test]
async fn global_resume_reads_independent_session_checkpoints_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let ready1 = dir.path().join("ses_1");
    let ready2 = dir.path().join("ses_2");
    let release = dir.path().join("release");
    std::fs::write(&config.opencode_bin,format!("#!/bin/sh\ntouch '{}/'$3\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '%s' '{{\"messages\":[]}}'\n",dir.path().display(),release.display())).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut other = agent();
    other.agent_session.as_mut().unwrap().value = "ses_2".into();
    for observed in [agent(), other.clone()] {
        bridge.state.mappings.insert(
            observed.key(),
            Mapping {
                group_id_hex: Some("bb".repeat(16)),
                active: true,
                output_initialised: true,
                agent_record: Some(observed),
                last_agent_status: "idle".into(),
                last_state_change_seq: 1,
                ..Mapping::default()
            },
        );
    }
    assert!(forwarding_command(&config, "desk").await.status.success());
    assert!(forwarding_command(&config, "away").await.status.success());
    let reconcile = tokio::spawn(async move {
        bridge.reconcile(vec![agent(), other]).await.unwrap();
        bridge
    });
    let concurrent = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !ready1.exists() || !ready2.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    std::fs::write(release, "").unwrap();
    let bridge = reconcile.await.unwrap();
    assert!(
        concurrent.is_ok(),
        "One slow export must not serialize every session checkpoint"
    );
    assert!(bridge.state.forwarding_enabled);
}

#[tokio::test]
async fn progress_is_edited_but_the_final_is_a_separate_message_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old answer", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    calls.lock().unwrap().clear();
    bridge.inbound(&inbound(HEX, "first-prompt")).await.unwrap();
    bridge.flush().await.unwrap();
    let export = json!({"messages":[
        {"info":{"id":"user","role":"user"},"parts":[]},
        {"info":{"id":"progress","role":"assistant"},"parts":[
            {"type":"text","text":"Private reasoning must stay local"},
            {"type":"tool","id":"todo-1","tool":"todowrite","state":{"status":"completed","input":{"todos":[
                {"content":"Inspect code","status":"completed","priority":"high"},
                {"content":"Run checks","status":"in_progress","priority":"high"}
            ]}}}
        ]}
    ]});
    std::fs::write(
        &config.opencode_bin,
        format!("#!/bin/sh\nprintf '%s' '{}'\n", export),
    )
    .unwrap();
    let mut working = agent();
    working.agent_status = "working".into();
    working.state_change_seq = 2;
    bridge.reconcile(vec![working]).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    bridge.flush().await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    export_script(&config.opencode_bin, "The final answer", "final");
    let mut done = agent();
    done.agent_status = "done".into();
    done.state_change_seq = 3;
    bridge.reconcile(vec![done.clone()]).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    bridge.flush().await.unwrap();
    bridge.reconcile(vec![done]).await.unwrap();
    bridge.flush().await.unwrap();
    let requests = calls.lock().unwrap();
    let sends: Vec<_> = requests
        .iter()
        .filter(|r| r["type"] == "send_final")
        .collect();
    assert_eq!(
        sends.len(),
        2,
        "One progress message and one immutable final reply"
    );
    assert_eq!(sends[0]["text"], "> working...");
    assert_eq!(sends[1]["text"], "The final answer");
    assert_ne!(sends[0]["idempotency_key"], sends[1]["idempotency_key"]);
    let edits: Vec<_> = requests
        .iter()
        .filter(|r| r["type"] == "edit_message")
        .collect();
    assert_eq!(edits.len(), 2);
    assert!(
        edits[0]["text"]
            .as_str()
            .unwrap()
            .contains("✓ Inspect code")
    );
    assert!(edits[0]["text"].as_str().unwrap().contains("▶ Run checks"));
    assert!(
        edits[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("> working...\n\n✓ Inspect code")
    );
    assert!(edits[1]["text"].as_str().unwrap().starts_with("> done"));
    assert!(
        edits[1]["text"]
            .as_str()
            .unwrap()
            .contains("✓ Inspect code")
    );
    assert!(
        edits
            .iter()
            .all(|edit| !edit["text"].as_str().unwrap().contains("\nTasks\n"))
    );
    assert!(
        !edits
            .iter()
            .any(|request| request["text"] == "The final answer")
    );
    assert!(
        edits
            .iter()
            .all(|r| r["target_message_id_hex"] == "cc".repeat(32))
    );
    assert!(!requests.iter().any(|r| {
        r["text"]
            .as_str()
            .is_some_and(|t| t.contains("Private reasoning"))
    }));
    assert!(bridge.state.outbox.is_empty());
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn an_uncertain_progress_send_does_not_delay_or_duplicate_the_final() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            last_agent_status: "idle".into(),
            final_ids: vec!["old".into()],
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    let mut working = agent();
    working.agent_status = "working".into();
    working.state_change_seq = 2;
    bridge.reconcile(vec![working]).await.unwrap();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let first = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        read_line(&mut tokio::io::BufReader::new(stream))
            .await
            .unwrap()
    });
    assert!(bridge.flush().await.is_err());
    let original = first.await.unwrap();
    std::fs::remove_file(&config.wn_agent_socket).unwrap();
    export_script(&config.opencode_bin, "Latest final", "new");
    bridge.reconcile(vec![agent()]).await.unwrap();
    let mut restarted = Bridge::open(config.clone()).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    restarted.flush().await.unwrap();
    restarted.flush().await.unwrap();
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "The final is independent of the uncertain progress send"
    );
    let requests = calls.lock().unwrap().clone();
    assert_eq!(requests[0]["text"], "Latest final");
    assert_ne!(requests[0]["idempotency_key"], original["idempotency_key"]);
    assert_eq!(requests[0]["type"], "send_final");
    assert_eq!(restarted.state.outbox.len(), 1);
    restarted.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    restarted.flush().await.unwrap();
    restarted.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    restarted.flush().await.unwrap();
    assert_eq!(
        restarted.state.outbox.len(),
        1,
        "Keep an unresolved target for inspection, never replay it after completion"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    mdk.abort();
}

#[tokio::test]
async fn each_new_prompt_gets_a_new_response_and_progress_is_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    calls.lock().unwrap().clear();
    bridge.inbound(&inbound(HEX, "first")).await.unwrap();
    bridge.flush().await.unwrap();
    bridge.flush().await.unwrap();
    export_script(&config.opencode_bin, "First answer", "first-final");
    let mut idle = agent();
    idle.state_change_seq = 2;
    bridge.reconcile(vec![idle.clone()]).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    bridge.flush().await.unwrap();
    bridge.inbound(&inbound(HEX, "second")).await.unwrap();
    bridge.flush().await.unwrap();
    let requests = calls.lock().unwrap();
    let sends: Vec<_> = requests
        .iter()
        .filter(|r| r["type"] == "send_final")
        .collect();
    assert_eq!(sends.len(), 3);
    assert_ne!(sends[0]["idempotency_key"], sends[2]["idempotency_key"]);
    assert_eq!(sends[1]["text"], "First answer");
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["type"] == "edit_message")
            .count(),
        1
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn long_final_replies_keep_all_text_without_waiting_for_progress_edits() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    export_script(&config.opencode_bin, "old", "old");
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some(group.clone()),
            final_ids: vec!["old".into()],
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.state.begin_response(&key, &group);
    let response = bridge.state.mappings[&key].response_id.clone().unwrap();
    bridge.flush().await.unwrap();
    let text = "🙂".repeat(10000);
    export_script(&config.opencode_bin, &text, "long-final");
    bridge.reconcile(vec![agent()]).await.unwrap();
    let throttle = |bridge: &mut Bridge, at| {
        let item = bridge
            .state
            .outbox
            .iter_mut()
            .find(|item| item.id == response);
        item.unwrap().response.as_mut().unwrap().last_attempt_at = at;
    };
    // The export can take longer than the 2 s edit throttle under load.
    throttle(&mut bridge, u64::MAX / 2);
    bridge.flush().await.unwrap();
    assert_eq!(
        calls.lock().unwrap().len(),
        3,
        "Both final parts precede the throttled terminal progress edit"
    );
    throttle(&mut bridge, 0);
    bridge.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls[1]["type"], "send_final");
    assert_eq!(calls[2]["type"], "send_final");
    assert_eq!(calls[3]["type"], "edit_message");
    assert_eq!(calls[3]["text"], "> done");
    assert_eq!(
        format!(
            "{}{}",
            calls[1]["text"].as_str().unwrap(),
            calls[2]["text"].as_str().unwrap()
        ),
        text
    );
    mdk.abort();
}

#[tokio::test]
async fn early_idle_keeps_the_working_checklist_until_the_final_reply() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.inbound(&inbound(HEX, "task-request")).await.unwrap();
    let key = agent().key();
    let response = bridge.state.mappings[&key].response_id.clone().unwrap();
    let write_tasks = |status: &str| {
        let export = json!({"messages":[
            {"info":{"id":"user","role":"user"},"parts":[{"type":"text","text":"Task request"}]},
            {"info":{"id":format!("tasks-{status}"),"role":"assistant"},"parts":[
                {"id":"todo","type":"tool","tool":"todowrite","state":{"status":"completed","input":{"todos":[{"content":"Run checks","status":status}]}}}
            ]}
        ]});
        std::fs::write(
            &config.opencode_bin,
            format!("#!/bin/sh\nprintf '%s' '{}'\n", export),
        )
        .unwrap();
    };
    write_tasks("in_progress");
    let mut working = agent();
    working.agent_status = "working".into();
    working.state_change_seq = 2;
    bridge.reconcile(vec![working.clone()]).await.unwrap();
    let progress = bridge
        .state
        .outbox
        .iter()
        .find(|item| item.id == response)
        .unwrap()
        .text
        .clone();
    assert_eq!(progress, "> working...\n\n▶ Run checks");
    let mut idle = agent();
    idle.state_change_seq = 3;
    bridge.reconcile(vec![idle.clone()]).await.unwrap();
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .find(|item| item.id == response)
            .unwrap()
            .text,
        progress
    );
    assert!(bridge.state.mappings[&key].pending_prompt);
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    // Tasks may still change while the runtime reports idle.
    write_tasks("completed");
    for _ in 0..100 {
        bridge.poll_sessions(vec![idle.clone()]).await.unwrap();
        if bridge
            .state
            .outbox
            .iter()
            .any(|item| item.id == response && item.text == "> working...\n\n✓ Run checks")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .find(|item| item.id == response)
            .unwrap()
            .text,
        "> working...\n\n✓ Run checks"
    );
    // Keep the same progress bubble when the runtime reports working again.
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    working.state_change_seq = 4;
    bridge.reconcile(vec![working]).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key].response_id.as_ref(),
        Some(&response)
    );
    bridge
        .state
        .outbox
        .iter_mut()
        .find(|item| item.id == response)
        .unwrap()
        .response
        .as_mut()
        .unwrap()
        .target = Some("sent-progress".into());
    export_script(&config.opencode_bin, "Final reply", "final");
    let mut done = agent();
    done.state_change_seq = 5;
    bridge.reconcile(vec![done]).await.unwrap();
    assert!(bridge.state.mappings[&key].response_id.is_none());
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .find(|item| item.id == response)
            .unwrap()
            .text,
        "> done\n\n✓ Run checks"
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "Final reply" && item.response.is_none())
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn late_final_is_resolved_before_opening_the_next_response() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    bridge.inbound(&inbound(HEX, "first")).await.unwrap();
    bridge.flush().await.unwrap();
    let first = bridge.state.mappings[&agent().key()]
        .response_id
        .clone()
        .unwrap();
    let mut idle = agent();
    idle.state_change_seq = 2;
    bridge.reconcile(vec![idle]).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()].pending_prompt,
        "Keep the first response associated with its late final"
    );
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .find(|item| item.id == first)
            .unwrap()
            .text,
        "> working..."
    );
    export_script(&config.opencode_bin, "First final", "first-final");
    bridge.inbound(&inbound(HEX, "second")).await.unwrap();
    let second = bridge.state.mappings[&agent().key()]
        .response_id
        .clone()
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .find(|m| m.id == first)
            .unwrap()
            .text,
        "> done"
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.response.is_none() && item.text == "First final")
    );
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .find(|m| m.id == second)
            .unwrap()
            .text,
        "> working..."
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .count(),
        2
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn an_ended_session_closes_an_unfinished_response_before_restoration() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let mut bridge = Bridge::open(config).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some(group.clone()),
            active: true,
            agent_record: Some(agent()),
            final_ids: vec!["old".into()],
            ..Mapping::default()
        },
    );
    bridge.state.begin_response(&key, &group);
    let first = bridge.state.mappings[&key].response_id.clone().unwrap();
    bridge.reconcile(vec![]).await.unwrap();
    assert!(bridge.state.mappings[&key].response_id.is_none());
    let closed = bridge.state.outbox.iter().find(|m| m.id == first).unwrap();
    assert!(closed.response.as_ref().unwrap().finished);
    assert!(closed.text.contains("ended"));
    let mut working = agent();
    working.agent_status = "working".into();
    bridge.reconcile(vec![working]).await.unwrap();
    assert_ne!(
        bridge.state.mappings[&key].response_id.as_ref(),
        Some(&first)
    );
}

#[tokio::test]
async fn edit_cooldown_also_starts_when_a_slow_request_completes() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    let calls = Arc::new(Mutex::new(vec![]));
    let captured = calls.clone();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            captured.lock().unwrap().push(request.clone());
            tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
            let response = json!({"id":request["id"],"marmot_agent_control":"marmot.agent-control.v2","type":"final_sent","message_ids_hex":["dd".repeat(32)]});
            reader
                .get_mut()
                .write_all((response.to_string() + "\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let mut bridge = Bridge::open(config).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some(group.clone()),
            ..Mapping::default()
        },
    );
    bridge.state.begin_response(&key, &group);
    let item = bridge.state.outbox.last_mut().unwrap();
    item.response.as_mut().unwrap().target = Some("cc".repeat(32));
    item.response.as_mut().unwrap().sent_text = Some("Earlier text".into());
    bridge.flush().await.unwrap();
    bridge.state.complete_progress(&key);
    bridge.flush().await.unwrap();
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "Do not race a final edit against the late-created progress edit"
    );
    server.abort();
}

#[tokio::test]
async fn clear_prompt_releases_an_awaiting_final_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let key = agent().key();
    let group = "bb".repeat(16);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some(group.clone()),
            agent_record: Some(agent()),
            active: true,
            pending_prompt: true,
            completion_pending: true,
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            final_ids: vec!["old".into()],
            ..Mapping::default()
        },
    );
    bridge.state.begin_response(&key, &group);
    bridge.save().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path,format!("phone_npub = {:?}\naccount_id_hex = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\nstate_path = {:?}\nopencode_bin = {:?}\ncodex_home = {:?}\nclaude_home = {:?}\nedit_responses = true\n",config.phone_npub,config.account_id_hex,config.herdr_socket,config.wn_agent_socket,config.state_path,config.opencode_bin,config.codex_home,config.claude_home)).unwrap();
    drop(bridge);
    let status = tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args(["--config", path.to_str().unwrap(), "clear-prompt", &key])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![agent()]).await.unwrap();
    assert!(!restarted.state.mappings[&key].pending_prompt);
    assert!(!restarted.state.mappings[&key].completion_pending);
    assert!(restarted.state.mappings[&key].response_id.is_none());
    assert!(
        restarted
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("cancelled"))
    );
}

#[tokio::test]
async fn a_failed_progress_edit_does_not_block_the_standalone_final() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "Final reply", "final");
    let calls = Arc::new(Mutex::new(vec![]));
    let captured = calls.clone();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            captured.lock().unwrap().push(request.clone());
            let mut response = if request["type"] == "edit_message" {
                json!({"type":"error","code":"app_error"})
            } else {
                json!({"type":"final_sent","message_ids_hex":["ee".repeat(32)]})
            };
            response["id"] = request["id"].clone();
            response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            reader
                .get_mut()
                .write_all((response.to_string() + "\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some(group.clone()),
            agent_record: Some(agent()),
            active: true,
            ..Mapping::default()
        },
    );
    bridge.state.begin_response(&key, &group);
    let progress = bridge.state.outbox.last_mut().unwrap();
    progress.response.as_mut().unwrap().target = Some("cc".repeat(32));
    progress.response.as_mut().unwrap().sent_text = Some("State: working".into());
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    let requests = calls.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["type"], "send_final");
    assert_eq!(requests[0]["text"], "Final reply");
    assert_eq!(requests[1]["type"], "edit_message");
    assert_eq!(requests[1]["text"], "> done");
    assert_eq!(
        bridge.state.outbox.len(),
        1,
        "A failed done edit must retain its target for recovery"
    );
    drop(bridge);
    server.abort();
    let _ = server.await;
    std::fs::remove_file(&config.wn_agent_socket).unwrap();
    let recovered = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    bridge.flush().await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    let requests = calls.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["type"] == "send_final")
            .count(),
        1
    );
    assert_eq!(
        requests.last().unwrap()["target_message_id_hex"],
        "cc".repeat(32)
    );
    assert_eq!(requests.last().unwrap()["text"], "> done");
    recovered.abort();
}

#[tokio::test]
async fn a_completed_progress_send_with_a_lost_ack_is_retained_without_creating_a_replacement() {
    let root = tempfile::tempdir().unwrap();
    let config = editable_config(root.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge
        .state
        .mappings
        .insert(key.clone(), activity_mapping(agent(), &group));
    bridge.state.begin_response(&key, &group);
    let id = bridge.state.mappings[&key].response_id.clone().unwrap();
    bridge.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .initial_text = Some("> working...".into());
    bridge.state.complete_progress(&key);
    assert_eq!(
        bridge.state.outbox.len(),
        1,
        "An attempted send may already be visible; retain it for target inspection"
    );
    bridge.save().unwrap();
    drop(bridge);
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(bridge.state.outbox.len(), 1);
    bridge.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    bridge.flush().await.unwrap();
    assert_eq!(bridge.state.outbox.len(), 1);
    assert_eq!(bridge.state.outbox[0].id, id);
    assert!(
        calls.lock().unwrap().is_empty(),
        "Do not assume an old transport deduplication receipt still exists"
    );
    assert!(forwarding_command(&config, "status").await.status.success());
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args([
            "--config",
            config
                .state_path
                .with_file_name("test-config.toml")
                .to_str()
                .unwrap(),
            "status",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let reported: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reported["unresolved_progress"], 1);
    server.abort();
}

#[tokio::test]
async fn mute_resume_keeps_one_progress_bubble_until_a_real_completed_reply() {
    for status in ["working", "idle"] {
        let root = tempfile::tempdir().unwrap();
        let config = editable_config(root.path());
        export_script(&config.opencode_bin, "Previous reply", "old-final");
        let calls = Arc::new(Mutex::new(vec![]));
        let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let mut observed = agent();
        observed.agent_status = status.into();
        let mut bridge = Bridge::open(config.clone()).unwrap();
        let key = observed.key();
        let group = "bb".repeat(16);
        let mut mapping = activity_mapping(observed.clone(), &group);
        mapping.last_agent_status = "working".into();
        mapping.final_ids.push("old-final".into());
        bridge.state.mappings.insert(key.clone(), mapping);
        bridge.state.begin_response(&key, &group);
        let id = bridge.state.mappings[&key].response_id.clone().unwrap();
        let delivery = bridge.state.outbox[0].response.as_mut().unwrap();
        delivery.target = Some("same-progress".into());
        delivery.initial_text = Some("> working...".into());
        delivery.sent_text = Some("> working...".into());
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        bridge.flush().await.unwrap();
        herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
        bridge.reconcile(vec![observed.clone()]).await.unwrap();
        bridge.flush().await.unwrap();
        assert_eq!(
            bridge.state.mappings[&key].response_id.as_deref(),
            Some(id.as_str())
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "Neither early idle nor resuming a working task recreates or completes progress"
        );
        export_script(&config.opencode_bin, "New reply", "new-final");
        observed.agent_status = "idle".into();
        observed.state_change_seq += 1;
        bridge.reconcile(vec![observed]).await.unwrap();
        bridge.flush().await.unwrap();
        let requests = calls.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["type"] == "send_final" && r["text"] == "New reply")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["type"] == "edit_message"
                    && r["target_message_id_hex"] == "same-progress"
                    && r["text"] == "> done")
                .count(),
            1
        );
        assert!(
            !requests
                .iter()
                .any(|r| r["type"] == "send_final" && r["text"] == "> working...")
        );
        server.abort();
    }
}

#[tokio::test]
async fn resume_remembers_a_completed_muted_reply_until_runtime_idle_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = editable_config(root.path());
    export_script(&config.opencode_bin, "Completed while muted", "muted-final");
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut working = agent();
    working.agent_status = "working".into();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = working.key();
    let group = "bb".repeat(16);
    let mut mapping = activity_mapping(working.clone(), &group);
    mapping.last_agent_status = "working".into();
    bridge.state.mappings.insert(key.clone(), mapping);
    bridge.state.begin_response(&key, &group);
    let delivery = bridge.state.outbox[0].response.as_mut().unwrap();
    delivery.target = Some("muted-progress".into());
    delivery.initial_text = Some("> working...".into());
    delivery.sent_text = Some("> working...".into());
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    bridge.flush().await.unwrap();
    herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
    bridge.reconcile(vec![working]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(calls.lock().unwrap().is_empty());
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "A completed muted reply observed before idle must still finish its progress later"
    );
    assert_eq!(calls[0]["type"], "edit_message");
    assert_eq!(calls[0]["text"], "> done");
    assert_eq!(calls[0]["target_message_id_hex"], "muted-progress");
    assert!(bridge.state.outbox.is_empty());
    server.abort();
}

#[tokio::test]
async fn muting_keeps_the_published_progress_target_and_resume_finishes_it_without_old_answers() {
    let root = tempfile::tempdir().unwrap();
    let config = editable_config(root.path());
    export_script(&config.opencode_bin, "Completed while muted", "muted-final");
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge
        .state
        .mappings
        .insert(key.clone(), activity_mapping(agent(), &group));
    bridge.state.begin_response(&key, &group);
    let id = bridge.state.mappings[&key].response_id.clone().unwrap();
    let delivery = bridge.state.outbox[0].response.as_mut().unwrap();
    delivery.target = Some("old-progress".into());
    delivery.initial_text = Some("> working...".into());
    delivery.sent_text = Some("> working...".into());
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key].response_id.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(bridge.state.outbox.len(), 1);
    assert!(calls.lock().unwrap().is_empty());
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["type"], "edit_message");
    assert_eq!(calls[0]["target_message_id_hex"], "old-progress");
    assert_eq!(calls[0]["text"], "> done");
    server.abort();
}

#[tokio::test]
async fn a_standalone_final_retries_with_its_own_stable_id_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    export_script(&config.opencode_bin, "Final reply", "final");
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    bridge.state.mappings.insert(
        key,
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            agent_record: Some(agent()),
            active: true,
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        read_line(&mut tokio::io::BufReader::new(stream))
            .await
            .unwrap()
    });
    assert!(bridge.flush().await.is_err());
    let original = server.await.unwrap();
    std::fs::remove_file(&config.wn_agent_socket).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![agent()]).await.unwrap();
    restarted.flush().await.unwrap();
    restarted.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["type"], "send_final");
    assert_eq!(calls[0]["text"], "Final reply");
    assert_eq!(calls[0]["idempotency_key"], original["idempotency_key"]);
    assert!(restarted.state.outbox.is_empty());
    server.abort();
}

#[tokio::test]
async fn detached_cancelled_progress_retains_its_uncertain_target_without_blocking_a_final() {
    let dir = tempfile::tempdir().unwrap();
    let config = editable_config(dir.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some(group.clone()),
            ..Mapping::default()
        },
    );
    bridge.state.begin_response(&key, &group);
    // Persisted after an initial send whose response was lost.
    bridge
        .state
        .outbox
        .last_mut()
        .unwrap()
        .response
        .as_mut()
        .unwrap()
        .initial_text = Some("State: working".into());
    bridge
        .state
        .update_response(&key, &group, "Response cancelled locally.", true);
    bridge.state.enqueue(&group, "The next final answer");
    bridge.save().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut restarted = Bridge::open(config).unwrap();
    restarted.flush().await.unwrap();
    assert_eq!(restarted.state.outbox.len(), 1);
    restarted.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    restarted.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "Send the final independently; do not replay an uncertain initial status"
    );
    assert_eq!(calls[0]["type"], "send_final");
    assert_eq!(calls[0]["text"], "The next final answer");
    assert_eq!(restarted.state.outbox.len(), 1);
    mdk.abort();
}

#[tokio::test]
async fn real_socket_round_trip_routes_one_group_and_only_new_final_answers() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(&config.opencode_bin, "historical private answer", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.flush().await.unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["type"] == "group_create_with_admins")
            .count(),
        1
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["type"] == "send_final")
            .all(|r| !r["text"]
                .as_str()
                .unwrap()
                .contains("historical private answer"))
    );
    bridge
        .inbound(&inbound(&"ff".repeat(32), "evil"))
        .await
        .unwrap();
    bridge.inbound(&inbound(HEX, "m1")).await.unwrap();
    bridge.inbound(&inbound(HEX, "m1")).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "agent.prompt")
            .count(),
        1
    );
    export_script(&config.opencode_bin, "new final", "new");
    let mut a = agent();
    a.state_change_seq = 3;
    bridge.reconcile(vec![a.clone()]).await.unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["text"] == "new final")
            .count(),
        1
    );
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![a]).await.unwrap();
    restarted.flush().await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["text"] == "new final")
            .count(),
        1
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn working_notice_remains_but_done_only_forwards_the_final_reply() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            last_agent_status: "idle".into(),
            last_state_change_seq: 1,
            final_ids: vec!["old".into()],
            pending_prompt: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    let mut working = agent();
    working.agent_status = "working".into();
    working.state_change_seq = 2;
    bridge.reconcile(vec![working.clone()]).await.unwrap();
    bridge.reconcile(vec![working]).await.unwrap();
    export_script(&config.opencode_bin, "Completed answer", "new");
    let mut done = agent();
    done.agent_status = "done".into();
    done.state_change_seq = 3;
    bridge.reconcile(vec![done.clone()]).await.unwrap();
    bridge.reconcile(vec![done]).await.unwrap();
    let texts: Vec<_> = bridge
        .state
        .outbox
        .iter()
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(
        texts.iter().filter(|text| **text == "> working...").count(),
        1
    );
    assert!(!texts.contains(&"> done"));
    assert_eq!(
        texts
            .iter()
            .filter(|text| **text == "Completed answer")
            .count(),
        1
    );
    assert_eq!(
        bridge.state.mappings[&agent().key()].last_agent_status,
        "done"
    );
    assert!(!bridge.state.mappings[&agent().key()].pending_prompt);
    assert!(!bridge.state.mappings[&agent().key()].completion_pending);
}

#[tokio::test]
async fn codex_session_round_trip_forwards_only_new_native_final_replies() {
    native_round_trip("codex", false).await;
}

#[tokio::test]
async fn claude_session_round_trip_forwards_only_new_native_final_replies() {
    native_round_trip("claude", false).await;
}

#[tokio::test]
async fn existing_native_groups_seed_history_once_without_recreating_the_group() {
    native_round_trip("codex", true).await;
    native_round_trip("claude", true).await;
}

async fn native_round_trip(provider: &str, legacy_mapping: bool) {
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut observed = agent();
    observed.agent = provider.into();
    observed.agent_session = Some(herdr_white_noise_bridge::herdr::Session {
        source: format!("herdr:{provider}"),
        agent: provider.into(),
        kind: "id".into(),
        value: ID.into(),
    });
    let path = if provider == "codex" {
        config
            .codex_home
            .join("sessions/2026/01/01")
            .join(format!("rollout-test-{ID}.jsonl"))
    } else {
        config
            .claude_home
            .join("projects/project")
            .join(format!("{ID}.jsonl"))
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let write = |new_reply: bool| {
        let mut rows = if provider == "codex" {
            vec![
                json!({"type":"session_meta","payload":{"id":ID}}),
                json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"old","last_agent_message":"Native old final"}}),
            ]
        } else {
            vec![
                json!({"type":"user","uuid":"u0","sessionId":ID,"message":{"role":"user","content":"Old prompt"}}),
                json!({"type":"assistant","uuid":"old","parentUuid":"u0","sessionId":ID,"message":{"role":"assistant","id":"api-old","stop_reason":"end_turn","content":[{"type":"text","text":"Native old final"}]}}),
            ]
        };
        if new_reply {
            if provider == "codex" {
                rows.push(json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"new","last_agent_message":"Native new final"}}));
            } else {
                rows.extend([
                json!({"type":"user","uuid":"u1","parentUuid":"old","sessionId":ID,"message":{"role":"user","content":"New prompt"}}),
                json!({"type":"assistant","uuid":"new","parentUuid":"u1","sessionId":ID,"message":{"role":"assistant","id":"api-new","stop_reason":"end_turn","content":[{"type":"text","text":"Native new final"}]}}),
            ]);
            }
        }
        std::fs::write(
            &path,
            rows.into_iter()
                .map(|row| row.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap();
    };
    write(false);
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer_for_agent(&config.herdr_socket, false, calls.clone(), observed.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    if legacy_mapping {
        bridge.state.mappings.insert(
            observed.key(),
            Mapping {
                group_id_hex: Some("bb".repeat(16)),
                active: true,
                agent_record: Some(observed.clone()),
                ..Mapping::default()
            },
        );
    }
    bridge.reconcile(vec![observed.clone()]).await.unwrap();
    bridge.flush().await.unwrap();
    bridge.inbound(&inbound(HEX, "new-prompt")).await.unwrap();
    assert!(bridge.state.mappings[&observed.key()].pending_prompt);
    write(true);
    observed.agent_status = "done".into();
    observed.state_change_seq += 1;
    bridge.reconcile(vec![observed.clone()]).await.unwrap();
    bridge.flush().await.unwrap();
    assert!(!bridge.state.mappings[&observed.key()].pending_prompt);
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![observed]).await.unwrap();
    restarted.flush().await.unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["method"] == "agent.prompt")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["text"] == "Native new final")
            .count(),
        1
    );
    assert!(
        !calls
            .iter()
            .any(|call| call["text"] == "Native old final" || call["text"] == "> done")
    );
    if legacy_mapping {
        assert!(
            !calls
                .iter()
                .any(|call| call["type"] == "group_create_with_admins")
        );
    }
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn legacy_native_session_that_exited_before_upgrade_does_not_replay_history() {
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    for provider in ["codex", "claude"] {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let mut observed = agent();
        observed.agent = provider.into();
        observed.agent_session = Some(herdr_white_noise_bridge::herdr::Session {
            source: format!("herdr:{provider}"),
            agent: provider.into(),
            kind: "id".into(),
            value: ID.into(),
        });
        let (path, rows) = if provider == "codex" {
            (
                config
                    .codex_home
                    .join("sessions/2026/01/01")
                    .join(format!("rollout-test-{ID}.jsonl")),
                vec![
                    json!({"type":"session_meta","payload":{"id":ID}}),
                    json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"old","last_agent_message":"Historical answer"}}),
                ],
            )
        } else {
            (
                config
                    .claude_home
                    .join("projects/project")
                    .join(format!("{ID}.jsonl")),
                vec![
                    json!({"type":"assistant","uuid":"old","sessionId":ID,"message":{"id":"api-old","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Historical answer"}]}}),
                ],
            )
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            rows.into_iter()
                .map(|row| row.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap();
        let key = observed.key();
        let mut bridge = Bridge::open(config).unwrap();
        bridge.state.mappings.insert(
            key.clone(),
            Mapping {
                group_id_hex: Some("bb".repeat(16)),
                active: true,
                agent_record: Some(observed),
                ..Mapping::default()
            },
        );
        bridge.reconcile(vec![]).await.unwrap();
        assert_eq!(bridge.state.outbox.len(), 1);
        assert!(bridge.state.outbox[0].text.starts_with("> ended"));
        assert!(bridge.state.mappings[&key].output_initialised);
        assert!(!bridge.state.mappings[&key].active);
    }
}

#[tokio::test]
async fn desk_selection_during_the_capability_probe_still_allows_an_invitation() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    export_script(&config.opencode_bin, "Historical answer", "old");
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let captured = Arc::new(Mutex::new(vec![]));
    let calls = captured.clone();
    let control = config.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            calls.lock().unwrap().push(request.clone());
            let mut response = if request["type"] == "group_create_capabilities" {
                assert!(forwarding_command(&control, "desk").await.status.success());
                json!({"type":"group_create_capabilities","initial_admins":true})
            } else {
                json!({"type":"group_created","group_id_hex":"bb".repeat(16)})
            };
            response["id"] = request["id"].clone();
            response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            reader
                .get_mut()
                .write_all((response.to_string() + "\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].group_id_hex.is_some());
    assert!(!bridge.state.mappings[&agent().key()].creating);
    assert!(!bridge.state.forwarding_enabled);
    assert!(bridge.state.outbox.is_empty());
    assert_eq!(captured.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn an_old_connector_cannot_leave_creation_intent_that_blocks_its_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    export_script(&config.opencode_bin, "Historical answer", "old");
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let captured = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            captured.lock().unwrap().push(request.clone());
            let response = json!({"marmot_agent_control":"marmot.agent-control.v2","id":request["id"],"type":"error","code":"control_error","message":"Unsupported request","retryable":false});
            reader
                .get_mut()
                .write_all((response.to_string() + "\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let mut bridge = Bridge::open(config).unwrap();
    for _ in 0..2 {
        bridge.reconcile(vec![agent()]).await.unwrap();
        let mapping = &bridge.state.mappings[&agent().key()];
        assert!(
            !mapping.creating,
            "An unsupported read-only capability probe is not an uncertain create"
        );
        assert!(mapping.group_id_hex.is_none());
    }
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request["type"] == "group_create_capabilities")
    );
    server.abort();
}

#[tokio::test]
async fn uncertain_group_creation_is_journalled_and_never_retried_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let first = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let capabilities = read_line(&mut reader).await.unwrap();
        assert_eq!(capabilities["type"], "group_create_capabilities");
        let response = json!({"marmot_agent_control":"marmot.agent-control.v2","id":capabilities["id"],"type":"group_create_capabilities","initial_admins":true});
        reader
            .get_mut()
            .write_all((response.to_string() + "\n").as_bytes())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let req = read_line(&mut tokio::io::BufReader::new(stream))
            .await
            .unwrap();
        assert_eq!(req["type"], "group_create_with_admins");
    });
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    first.await.unwrap();
    assert!(BridgeState::load(&config.state_path).unwrap().mappings[&agent().key()].creating);
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![agent()]).await.unwrap();
    assert!(
        restarted.state.mappings[&agent().key()]
            .group_id_hex
            .is_none()
    );
}

#[test]
fn state_is_private_and_utf8_chunks_preserve_all_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut state = BridgeState::default();
    let text = "🙂".repeat(16000);
    state.enqueue("group", &text);
    state.save(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(state.outbox.iter().all(|i| i.text.len() <= 30000));
    assert_eq!(
        state
            .outbox
            .iter()
            .map(|i| i.text.as_str())
            .collect::<String>(),
        text
    );
}

#[tokio::test]
async fn response_id_mismatch_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"{\"id\":\"wrong\",\"result\":{}}\n")
            .await
            .unwrap();
    });
    assert!(Socket { path }.herdr("ping", json!({})).await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn ended_groups_do_not_route_to_a_replacement_agent() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![]).await.unwrap();
    bridge.inbound(&inbound(HEX, "later")).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.contains("No prompt was sent"))
    );
}

#[tokio::test]
async fn session_change_on_an_established_terminal_creates_a_separate_chat_without_moving_work() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let original = agent();
    let mut replacement = original.clone();
    replacement.agent_session.as_mut().unwrap().value = "ses_replacement".into();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    // The connector fixture creates "bb"; seed a distinct original group.
    let mut mapping = activity_mapping(original.clone(), &"cc".repeat(16));
    mapping.pending_prompt = true;
    mapping.final_ids.push("old".into());
    mapping
        .seen_messages
        .push("original-delivery-receipt".into());
    mapping
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "accepted-original-delivery".into(),
            group: "cc".repeat(16),
            text: "Accepted work for the original session only".into(),
            media: None,
        });
    mapping.queue_flight = Some(herdr_white_noise_bridge::state::QueueFlight {
        before_final: Some("old".into()),
        confirmed: true,
    });
    let accepted = mapping.prompt_queue.clone();
    let flight = mapping.queue_flight.clone();
    bridge.state.mappings.insert(original.key(), mapping);
    bridge.reconcile(vec![replacement.clone()]).await.unwrap();
    assert!(bridge.agents.contains_key(&replacement.key()));
    let new_group = bridge.state.mappings[&replacement.key()]
        .group_id_hex
        .clone()
        .unwrap();
    assert_ne!(new_group, "cc".repeat(16));
    assert!(
        bridge.state.mappings[&replacement.key()]
            .session_change_from
            .is_none()
    );
    assert!(
        bridge.state.mappings[&replacement.key()]
            .prompt_queue
            .is_empty()
    );
    assert!(
        bridge.state.mappings[&replacement.key()]
            .queue_flight
            .is_none()
    );
    assert!(
        !bridge.state.mappings[&replacement.key()]
            .seen_messages
            .contains(&"original-delivery-receipt".into())
    );
    assert!(!bridge.state.mappings[&original.key()].active);
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![replacement.clone()]).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&replacement.key()]
            .group_id_hex
            .as_deref(),
        Some(new_group.as_str())
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["type"] == "group_create_with_admins")
            .count(),
        1
    );
    let mut text = inbound(HEX, "paused-phone-prompt");
    text["group_id_hex"] = json!("cc".repeat(16));
    text["message"]["text"] = json!("Do not send this to the replacement");
    bridge.inbound(&text).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.contains("No prompt was sent"))
    );
    assert_eq!(
        bridge.state.mappings[&original.key()].prompt_queue,
        accepted
    );
    assert_eq!(bridge.state.mappings[&original.key()].queue_flight, flight);
    assert!(
        bridge.state.mappings[&original.key()]
            .seen_messages
            .contains(&"original-delivery-receipt".into())
    );
    bridge.reconcile(vec![original.clone()]).await.unwrap();
    assert!(bridge.agents.contains_key(&original.key()));
    assert_eq!(
        bridge.state.mappings[&original.key()].group_id_hex,
        Some("cc".repeat(16))
    );
    assert_eq!(
        bridge.state.mappings[&replacement.key()].group_id_hex,
        Some(new_group)
    );
    server.abort();
}

#[tokio::test]
async fn session_change_guard_allows_an_intentional_new_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let original = agent();
    let mut launched = original.clone();
    launched.terminal_id = "new-terminal".into();
    launched.pane_id = "w1:p2".into();
    launched.agent_session.as_mut().unwrap().value = "ses_launched".into();
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .mappings
        .insert(original.key(), activity_mapping(original, &"bb".repeat(16)));
    bridge
        .state
        .pending_chat_agents
        .insert(launched.terminal_id.clone(), "opencode".into());
    bridge.reconcile(vec![launched.clone()]).await.unwrap();
    assert!(
        bridge.state.mappings[&launched.key()]
            .group_id_hex
            .is_some()
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "group_create_with_admins")
    );
    server.abort();
}

#[tokio::test]
async fn session_change_creates_a_chat_while_muted_but_not_without_a_native_identity() {
    for verified in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        export_script(&config.opencode_bin, "old", "old");
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        let calls = Arc::new(Mutex::new(vec![]));
        let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let original = agent();
        let mut replacement = original.clone();
        replacement.tab_label = Some("new-task".into());
        replacement.workspace_label = Some("work".into());
        if verified {
            replacement.agent_session.as_mut().unwrap().value = "ses_new".into();
        } else {
            replacement.agent_session = None;
            replacement.agent_status = "unknown".into();
        }
        let mut bridge = Bridge::open(config).unwrap();
        bridge.state.mappings.insert(
            original.key(),
            activity_mapping(original.clone(), &"cc".repeat(16)),
        );
        bridge.reconcile(vec![replacement.clone()]).await.unwrap();
        assert!(!bridge.state.forwarding_enabled);
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["type"] == "group_create_with_admins")
                .count(),
            usize::from(verified)
        );
        if verified {
            assert_eq!(
                bridge.state.mappings[&replacement.key()]
                    .group_name
                    .as_deref(),
                Some("new-task (work)")
            );
            assert!(!bridge.state.mappings[&original.key()].active);
        } else {
            assert!(!bridge.state.mappings.contains_key(&replacement.key()));
            assert!(bridge.state.mappings[&original.key()].active);
        }
        assert_eq!(
            bridge.state.mappings[&original.key()]
                .group_id_hex
                .as_deref(),
            Some("cccccccccccccccccccccccccccccccc")
        );
        server.abort();
    }
}

#[tokio::test]
async fn session_change_rejects_duplicate_native_identities_before_guard_mutations() {
    for reverse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        export_script(&config.opencode_bin, "old", "old");
        let calls = Arc::new(Mutex::new(vec![]));
        let connector = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let original = agent();
        let mut replacement = original.clone();
        replacement.agent_session.as_mut().unwrap().value = "ses_replacement".into();
        let mut fresh = replacement.clone();
        fresh.terminal_id = "fresh-terminal".into();
        fresh.pane_id = "w1:p2".into();
        let mut bridge = Bridge::open(config).unwrap();
        bridge
            .state
            .mappings
            .insert(original.key(), activity_mapping(original, &"bb".repeat(16)));
        let before = bridge.state.clone();
        let mut snapshot = vec![fresh, replacement];
        if reverse {
            snapshot.reverse();
        }
        assert!(bridge.reconcile(snapshot).await.is_err());
        assert_eq!(bridge.state, before);
        assert!(calls.lock().unwrap().is_empty());
        connector.abort();
    }
}

#[tokio::test]
async fn session_change_confirmation_rechecks_live_identity_and_seeds_history() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(
        &config.opencode_bin,
        "already in the replacement",
        "prior-replacement-final",
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let connector = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let original = agent();
    let mut replacement = original.clone();
    replacement.agent_session.as_mut().unwrap().value = "ses_replacement".into();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let mut mapping = activity_mapping(original.clone(), &"bb".repeat(16));
    mapping.final_ids.push("prior-replacement-final".into());
    bridge.state.mappings.insert(original.key(), mapping);
    // Old saved holds remain explicit approvals; new discoveries no longer make holds.
    bridge.state.mappings.insert(
        replacement.key(),
        Mapping {
            agent_record: Some(replacement.clone()),
            session_change_from: Some(original.key()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![replacement.clone()]).await.unwrap();
    // Restoring the original before confirmation must reject the stale candidate.
    let native = peer_for_agent(
        &config.herdr_socket,
        false,
        Arc::new(Mutex::new(vec![])),
        original.clone(),
    )
    .await;
    assert!(
        bridge
            .confirm_session_change(&replacement.key())
            .await
            .is_err()
    );
    assert!(
        bridge.state.mappings[&replacement.key()]
            .session_change_from
            .is_some()
    );
    native.abort();
    let _ = native.await;
    std::fs::remove_file(&config.herdr_socket).unwrap();
    let native = peer_for_agent(
        &config.herdr_socket,
        false,
        Arc::new(Mutex::new(vec![])),
        replacement.clone(),
    )
    .await;
    bridge
        .confirm_session_change(&replacement.key())
        .await
        .unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    assert!(
        bridge.state.mappings[&replacement.key()]
            .session_change_from
            .is_none()
    );
    bridge.reconcile(vec![replacement.clone()]).await.unwrap();
    assert!(
        bridge.state.mappings[&replacement.key()]
            .group_id_hex
            .is_some()
    );
    assert!(
        bridge.state.mappings[&replacement.key()]
            .final_ids
            .contains(&"prior-replacement-final".into())
    );
    assert!(
        bridge.state.mappings[&original.key()]
            .group_id_hex
            .is_some()
    );
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .all(|m| m.text != "already in the replacement")
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["type"] == "group_create_with_admins")
            .count(),
        1
    );
    native.abort();
    connector.abort();
}

#[tokio::test]
async fn a_missing_connector_before_submission_retries_when_it_appears() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(&config.opencode_bin, "old", "old");
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(!bridge.state.mappings[&agent().key()].creating);
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].group_id_hex.is_some());
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["group_create_capabilities", "group_create_with_admins"]
    );
    server.abort();
}

#[tokio::test]
async fn failed_prompt_preflight_does_not_lock_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut bridge = Bridge::open(config).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(agent().key(), agent());
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            ..Mapping::default()
        },
    );
    assert!(bridge.inbound(&inbound(HEX, "m1")).await.is_err());
    assert!(!bridge.state.mappings[&agent().key()].pending_prompt);
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .is_empty()
    );
}

#[tokio::test]
async fn an_outage_defers_phone_input_without_consuming_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(dir.path())).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            ..Mapping::default()
        },
    );
    assert!(bridge.inbound(&inbound(HEX, "offline")).await.is_err());
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .is_empty()
    );
    assert!(bridge.state.outbox.is_empty());
}

#[tokio::test]
async fn truncated_recovered_input_never_reaches_herdr() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(dir.path())).unwrap();
    bridge.herdr_available = true;
    bridge.agents.insert(agent().key(), agent());
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            ..Mapping::default()
        },
    );
    let mut event = inbound(HEX, "long");
    event["message"]["text_truncated"] = json!(true);
    bridge.inbound(&event).await.unwrap();
    assert!(!bridge.state.mappings[&agent().key()].pending_prompt);
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.contains("truncated"))
    );
}

#[tokio::test]
async fn final_output_is_collected_even_if_the_agent_exits_between_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    export_script(&config.opencode_bin, "last answer", "last");
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![]).await.unwrap();
    assert_eq!(bridge.state.outbox[0].text, "last answer");
    assert!(bridge.state.outbox[1].text.starts_with("> ended"));
    bridge.reconcile(vec![]).await.unwrap();
    assert_eq!(bridge.state.outbox.len(), 2);
}

#[tokio::test]
async fn delayed_native_identity_does_not_create_a_provisional_opencode_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(dir.path())).unwrap();
    let mut a = agent();
    a.agent_session = None;
    bridge.reconcile(vec![a]).await.unwrap();
    assert!(bridge.state.mappings.is_empty());
}

#[tokio::test]
async fn consumed_message_ids_are_not_evicted_while_history_can_replay_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(dir.path())).unwrap();
    bridge.herdr_available = true;
    let mut event = inbound(HEX, "original");
    event["message"]["text"] = json!("/status");
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            seen_messages: (0..4096).map(|i| i.to_string()).collect(),
            ..Mapping::default()
        },
    );
    bridge.inbound(&event).await.unwrap();
    assert_eq!(bridge.state.mappings[&agent().key()].seen_messages[0], "0");
    assert_eq!(
        bridge.state.mappings[&agent().key()].seen_messages.len(),
        4097
    );
}

#[tokio::test]
async fn failed_session_export_does_not_starve_a_healthy_session() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::write(
        &config.opencode_bin,
        "#!/bin/sh\ncase \"$3\" in bad) exit 1;; esac\nprintf '%s' '{\"messages\":[]}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    let mut bad = agent();
    bad.agent_session.as_mut().unwrap().value = "bad".into();
    bridge.reconcile(vec![bad, agent()]).await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].group_id_hex.is_some());
    server.abort();
}

#[tokio::test]
async fn recovery_uses_real_cursors_pages_all_history_and_recovers_late_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let rows: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new((0..60).map(|i|json!({"message_id_hex":format!("{i:064x}"),"recorded_at":100+i,"availability":"available","kind":9,"sender":{"account_id_hex":HEX},"text":"/status","attachments":[]})).collect()));
    let data = rows.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let req = read_line(&mut reader).await.unwrap();
            assert_eq!(req["type"], "timeline_list");
            assert!(req.get("after").is_none());
            let mut response = if req["group_id_hex"] == "dd".repeat(16) {
                json!({"type":"error","code":"app_error"})
            } else {
                let rows = data.lock().unwrap();
                let end = if let Some(cursor) = req.get("before") {
                    rows.iter()
                        .position(|row| row["message_id_hex"] == cursor["message_id_hex"])
                        .expect("MDK requires an existing cursor row")
                } else {
                    rows.len()
                };
                let start = end.saturating_sub(50);
                json!({"type":"timeline_page","messages":rows[start..end],"has_more_before":start>0,"has_more_after":false})
            };
            response["id"] = req["id"].clone();
            response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            reader.get_mut().write_all(&bytes).await.unwrap();
        }
    });
    let mut bridge = Bridge::open(config).unwrap();
    bridge.herdr_available = true;
    bridge.state.mappings.insert(
        "healthy".into(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            joined_at: u64::MAX,
            ..Mapping::default()
        },
    );
    bridge.recover_inbound().await.unwrap();
    assert_eq!(bridge.state.mappings["healthy"].seen_messages.len(), 60);
    let mut late = rows.lock().unwrap()[0].clone();
    late["message_id_hex"] = json!("ee".repeat(32));
    late["recorded_at"] = json!(1);
    rows.lock().unwrap().insert(0, late);
    bridge.state.mappings.insert(
        "failing-first".into(),
        Mapping {
            group_id_hex: Some("dd".repeat(16)),
            ..Mapping::default()
        },
    );
    assert!(bridge.recover_inbound().await.is_err());
    assert_eq!(bridge.state.mappings["healthy"].seen_messages.len(), 61);
    server.abort();
}

#[tokio::test]
async fn failed_export_does_not_repeat_a_lifecycle_notice() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::write(&config.opencode_bin, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            last_agent_status: "working".into(),
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(
        bridge
            .state
            .outbox
            .iter()
            .filter(|m| m.text == "> idle")
            .count(),
        1
    );
}

#[tokio::test]
async fn temporarily_missing_backend_keeps_an_existing_session_present() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(dir.path())).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    let mut pending = agent();
    pending.agent = String::new();
    bridge.reconcile(vec![pending]).await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].active);
    assert!(
        !bridge
            .state
            .outbox
            .iter()
            .any(|m| m.text.starts_with("> ended"))
    );
}

#[tokio::test]
async fn successful_export_retry_releases_a_completed_prompt_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::write(&config.opencode_bin, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            pending_prompt: true,
            last_agent_status: "working".into(),
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    bridge.reconcile(vec![agent()]).await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].completion_pending);
    export_script(&config.opencode_bin, "Recovered final", "final");
    let mut restarted = Bridge::open(config).unwrap();
    restarted.reconcile(vec![agent()]).await.unwrap();
    assert!(!restarted.state.mappings[&agent().key()].pending_prompt);
    assert!(!restarted.state.mappings[&agent().key()].completion_pending);
    assert_eq!(
        restarted
            .state
            .outbox
            .iter()
            .filter(|m| m.text == "Recovered final")
            .count(),
        1
    );
}

#[tokio::test]
async fn unknown_native_identity_never_reuses_an_ended_group_or_consumes_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::open(config(dir.path())).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            agent_record: Some(agent()),
            ..Mapping::default()
        },
    );
    let mut uncertain = agent();
    uncertain.agent_session = None;
    uncertain.agent = "claude".into();
    bridge.reconcile(vec![uncertain]).await.unwrap();
    assert!(bridge.state.mappings[&agent().key()].active);
    assert!(bridge.inbound(&inbound(HEX, "uncertain")).await.is_err());
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .is_empty()
    );
    assert_eq!(bridge.state.mappings.len(), 1);
}

#[tokio::test]
async fn renaming_existing_groups_keeps_their_mapping_and_skips_matching_names() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let mdk = tokio::spawn(async move {
        let mut subject = "[cd] Review".to_string();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let req = read_line(&mut reader).await.unwrap();
            captured.lock().unwrap().push(req.clone());
            let mut response = match req["type"].as_str().unwrap() {
                "group_info" => {
                    json!({"type":"group_info","subject":subject})
                }
                "group_profile_update" => {
                    assert_eq!(req["group_id_hex"], "bb".repeat(16));
                    let description = req["description"].as_str().unwrap();
                    assert!(description.contains("Agent: opencode"));
                    assert!(description.contains("Workspace:"));
                    subject = req["name"].as_str().unwrap().to_owned();
                    json!({"type":"group_profile_updated","message_ids_hex":["cc".repeat(32)]})
                }
                _ => panic!("Group rename must not create or replace a group"),
            };
            response["id"] = req["id"].clone();
            response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            reader.get_mut().write_all(&bytes).await.unwrap();
        }
    });
    let herdr = peer(&config.herdr_socket, false, Arc::new(Mutex::new(vec![]))).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            agent_record: Some(agent()),
            seen_messages: vec!["consumed-phone-message".into()],
            prompt_queue: vec![herdr_white_noise_bridge::state::QueuedPrompt {
                id: "queued-phone-message".into(),
                group: "bb".repeat(16),
                text: "Keep this queued prompt".into(),
                media: None,
            }],
            ..Mapping::default()
        },
    );
    let original = bridge.state.clone();
    bridge.rename_groups().await.unwrap();
    bridge.rename_groups().await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .group_description
            .is_some()
    );
    let mut after = bridge.state.clone();
    after
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .group_description = None;
    after.mappings.get_mut(&agent().key()).unwrap().group_name = None;
    assert_eq!(after, original);
    let requests = requests.lock().unwrap();
    let updates: Vec<_> = requests
        .iter()
        .filter(|r| r["type"] == "group_profile_update")
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["name"], "Review");
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn renaming_uses_the_existing_title_when_an_ended_group_has_no_stored_tab_label() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let calls = Arc::new(Mutex::new(vec![]));
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut old = agent();
    old.tab_id.clear();
    old.tab_label = None;
    old.agent_session.as_mut().unwrap().value = "ses_ended".into();
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        old.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            agent_record: Some(old),
            ..Mapping::default()
        },
    );
    bridge.rename_groups().await.unwrap();
    assert!(calls.lock().unwrap().iter().any(
        |request| request["type"] == "group_profile_update" && request["name"] == "historical"
    ));
    bridge.rename_groups().await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["type"] == "group_profile_update")
            .count(),
        1,
        "Repeating migration must not change an already migrated title"
    );
    herdr.abort();
    mdk.abort();
}

#[tokio::test]
async fn deferred_prepared_media_does_not_block_another_session() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    export_script(&config.opencode_bin, "Historical answer", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    let mut uncertain = bridge.state.mappings[&agent().key()].clone();
    uncertain.group_id_hex = Some("dd".repeat(16));
    bridge
        .state
        .mappings
        .insert("uncertain-session".into(), uncertain);
    let mut deferred = inbound(HEX, "deferred-media");
    deferred["group_id_hex"] = json!("dd".repeat(16));
    deferred["_codr_prepared_text"] = json!("Deferred prompt");
    let mut ready = inbound(HEX, "ready-media");
    let image = root.path().join("layout.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nexample").unwrap();
    ready["message"]["text"] = json!("Ready prompt");
    ready["message"]["media"] = json!([{"media_type":"image/png","test_path":image,"file_name":"layout.png","source_epoch":1,
        "ciphertext_sha256":"aa".repeat(32),"plaintext_sha256":"bb".repeat(32),"nonce_hex":"00".repeat(12),
        "version":"encrypted-media-v1","source_epoch":1,"locators":[{"kind":"blossom-v1","value":"https://example.com/fixture"}]}]);
    bridge.state.media_pending = vec![deferred, ready];
    assert!(bridge.process_media().await.is_err());
    for _ in 0..1000 {
        // Each worker tick can arrive after the deferred session's retry deadline.
        bridge.state.media_pending[0]["_codr_retry_at"] = json!(0);
        let _ = bridge.process_media().await;
        if bridge.state.media_pending.len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(bridge.state.media_pending.len(), 1);
    assert_eq!(
        bridge.state.media_pending[0]["message"]["message_id_hex"],
        "deferred-media"
    );
    let requests = calls.lock().unwrap();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|call| call["method"] == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0]["params"]["text"]
            .as_str()
            .unwrap()
            .starts_with("Ready prompt\n\nAttached image: ")
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn prepared_audio_survives_a_herdr_outage_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    export_script(&config.opencode_bin, "Historical answer", "old");
    let command = root.path().join("transcribe");
    std::fs::write(
        &command,
        "#!/bin/sh\nprintf 'Inspect the blue button.\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.transcribe_command = vec![command.to_string_lossy().into_owned()];
    let recording = root.path().join("voice.ogg");
    std::fs::write(&recording, "audio fixture").unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    let mut event = inbound(HEX, "audio-outage");
    event["message"]["text"] = json!("");
    event["message"]["media"] = json!([{"media_type":"audio/ogg","test_path":recording,"file_name":"voice.ogg",
        "ciphertext_sha256":"aa".repeat(32),"plaintext_sha256":"bb".repeat(32),"nonce_hex":"00".repeat(12),
        "version":"encrypted-media-v1","source_epoch":1,"locators":[{"kind":"blossom-v1","value":"https://example.com/fixture"}]}]);
    bridge.inbound(&event).await.unwrap();
    bridge.process_media().await.unwrap();
    bridge.herdr_available = false;
    for _ in 0..1000 {
        if bridge.process_media().await.is_err() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        BridgeState::load(&config.state_path)
            .unwrap()
            .media_pending
            .len(),
        1
    );
    assert!(
        !bridge.state.mappings[&agent().key()]
            .seen_messages
            .contains(&"audio-outage".into())
    );
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    for _ in 0..1000 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(bridge.state.media_pending.is_empty());
    bridge.inbound(&event).await.unwrap();
    let requests = calls.lock().unwrap();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|call| call["method"] == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["params"]["text"], "Inspect the blue button.");
    assert_eq!(
        requests
            .iter()
            .filter(|call| call["type"] == "download_media")
            .count(),
        1
    );
    mdk.abort();
    herdr.abort();
}

#[tokio::test]
async fn a_transcript_notice_survives_lost_send_and_edit_responses_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let listener = UnixListener::bind(&config.wn_agent_socket).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let server = tokio::spawn(async move {
        for attempt in 0..4 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            captured.lock().unwrap().push(request.clone());
            if attempt == 0 || attempt == 2 {
                continue; // The send/edit may have succeeded, but its reply was lost.
            }
            let response = json!({"marmot_agent_control":"marmot.agent-control.v2","id":request["id"],"type":"final_sent","message_ids_hex":["cc".repeat(32)]});
            reader
                .get_mut()
                .write_all((response.to_string() + "\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let id = bridge.state.begin_transcription("bb").unwrap();
    assert!(bridge.flush().await.is_err());
    let item = bridge
        .state
        .outbox
        .iter_mut()
        .find(|item| item.id == id)
        .unwrap();
    item.text = "> transcribed\n\n> A spoken request.".into();
    item.response.as_mut().unwrap().finished = true;
    item.response.as_mut().unwrap().last_attempt_at = 0;
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.flush().await.unwrap();
    let delivery = bridge.state.outbox[0].response.as_mut().unwrap();
    assert_eq!(delivery.target.as_deref(), Some("cc".repeat(32).as_str()));
    delivery.last_attempt_at = 0;
    assert!(bridge.flush().await.is_err());
    assert_eq!(bridge.state.outbox.len(), 1);
    bridge.state.outbox[0]
        .response
        .as_mut()
        .unwrap()
        .last_attempt_at = 0;
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.flush().await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0]["type"], "send_final");
    assert_eq!(calls[1]["type"], "send_final");
    assert_eq!(calls[0]["idempotency_key"], calls[1]["idempotency_key"]);
    assert_eq!(calls[0]["text"], "> transcribing");
    assert_eq!(calls[1]["text"], "> transcribing");
    for call in &calls[2..] {
        assert_eq!(call["type"], "edit_message");
        assert_eq!(call["target_message_id_hex"], "cc".repeat(32));
        assert_eq!(call["text"], "> transcribed\n\n> A spoken request.");
    }
    server.abort();
}

#[tokio::test]
async fn a_transcript_ready_before_its_first_send_uses_one_completed_bubble() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let calls = Arc::new(Mutex::new(vec![]));
    let server = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    let id = bridge.state.begin_transcription("bb").unwrap();
    let item = bridge
        .state
        .outbox
        .iter_mut()
        .find(|item| item.id == id)
        .unwrap();
    item.text = "> transcribed\n\n> Already prepared.".into();
    item.response.as_mut().unwrap().finished = true;
    bridge.flush().await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["type"], "send_final");
    assert_eq!(calls[0]["text"], "> transcribed\n\n> Already prepared.");
    server.abort();
}

#[tokio::test]
async fn audio_queue_survives_restart_and_submits_the_transcript_once_without_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.edit_responses = true;
    export_script(&config.opencode_bin, "Historical answer", "old");
    let command = root.path().join("transcribe");
    std::fs::write(
        &command,
        "#!/bin/sh\nprintf 'Inspect the blue button.\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.transcribe_command = vec![command.to_string_lossy().into_owned()];
    let recording = root.path().join("voice.ogg");
    std::fs::write(&recording, "audio fixture").unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let herdr = peer(&config.herdr_socket, false, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    let mut event = inbound(HEX, "audio-one");
    event["message"]["text"] = json!("");
    event["message"]["media"] = json!([{"media_type":"audio/ogg","test_path":recording,"file_name":"voice.ogg",
        "ciphertext_sha256":"aa".repeat(32),"plaintext_sha256":"bb".repeat(32),"nonce_hex":"00".repeat(12),
        "version":"encrypted-media-v1","source_epoch":1,"locators":[{"kind":"blossom-v1","value":"https://example.com/fixture"}]}]);
    let mut foreign = event.clone();
    foreign["message"]["sender"]["account_id_hex"] = json!("ff".repeat(32));
    bridge.inbound(&foreign).await.unwrap();
    assert!(bridge.state.media_pending.is_empty());
    let mut summary = event.clone();
    summary["message"]["media"] = json!([{"media_type":"audio/ogg","file_name":"voice.ogg"}]);
    bridge.inbound(&summary).await.unwrap();
    bridge.inbound(&summary).await.unwrap();
    assert_eq!(bridge.state.media_pending.len(), 1);
    assert!(
        !bridge.state.mappings[&agent().key()]
            .seen_messages
            .contains(&"audio-one".into())
    );
    bridge.flush().await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![agent()]).await.unwrap();
    for _ in 0..2 {
        for _ in 0..1000 {
            bridge.process_media().await.unwrap();
            if bridge.state.media_pending[0]["_codr_retry_at"]
                .as_u64()
                .unwrap_or(0)
                > now()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            bridge.state.media_pending[0]["_codr_retry_at"]
                .as_u64()
                .unwrap_or(0)
                > now()
        );
        assert!(
            !bridge.state.mappings[&agent().key()]
                .seen_messages
                .contains(&"audio-one".into())
        );
        for _ in 0..3 {
            bridge.inbound(&summary).await.unwrap();
            bridge.process_media().await.unwrap();
        }
        assert_eq!(
            bridge
                .state
                .outbox
                .iter()
                .filter(|m| m.text == "> transcribing")
                .count(),
            1,
            "Unavailable history references must not repeat the preparation notice"
        );
        bridge.state.media_pending[0]["_codr_retry_at"] = json!(0);
    }
    bridge.process_media().await.unwrap();
    // An old MDK returns summaries during recovery. A complete live event must
    // hydrate that queued row, not be discarded as a duplicate.
    bridge.inbound(&event).await.unwrap();
    for _ in 0..1000 {
        bridge.process_media().await.unwrap();
        if bridge.state.media_pending.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(bridge.state.media_pending.is_empty());
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .contains(&"audio-one".into())
    );
    bridge.inbound(&event).await.unwrap();
    // The original bubble target and completed transcript edit survive restart.
    for item in &mut bridge.state.outbox {
        if let Some(delivery) = &mut item.response {
            delivery.last_attempt_at = 0;
        }
    }
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.flush().await.unwrap();
    let requests = calls.lock().unwrap();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|call| call["method"] == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["params"]["text"], "Inspect the blue button.");
    assert_eq!(
        requests
            .iter()
            .filter(|call| call["type"] == "send_final" && call["text"] == "> transcribing")
            .count(),
        1
    );
    let edits: Vec<_> = requests
        .iter()
        .filter(|call| {
            call["type"] == "edit_message"
                && call["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("> transcribed"))
        })
        .collect();
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0]["text"],
        "> transcribed\n\n> Inspect the blue button."
    );
    assert_eq!(edits[0]["target_message_id_hex"], "cc".repeat(32));
    assert_eq!(
        requests
            .iter()
            .filter(|call| call["type"] == "download_media")
            .count(),
        1
    );
    mdk.abort();
    herdr.abort();
}

struct GroupProfile {
    subject: String,
    description: String,
    reject_updates: bool,
}

async fn group_profile_peer(
    path: &std::path::Path,
    profile: Arc<Mutex<GroupProfile>>,
    calls: Arc<Mutex<Vec<Value>>>,
    state_path: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let req = read_line(&mut reader).await.unwrap();
            calls.lock().unwrap().push(req.clone());
            let mut response = match req["type"].as_str().unwrap() {
                "group_info" => {
                    let profile = profile.lock().unwrap();
                    json!({"type":"group_info","subject":profile.subject,"description":profile.description})
                }
                "group_profile_update" => {
                    let saved: BridgeState =
                        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
                    assert!(
                        saved
                            .mappings
                            .values()
                            .any(|mapping| mapping.rename_uncertain)
                    );
                    let mut profile = profile.lock().unwrap();
                    if profile.reject_updates {
                        json!({"type":"error","error":"uncertain"})
                    } else {
                        profile.subject = req["name"].as_str().unwrap().to_owned();
                        profile.description = req["description"].as_str().unwrap().to_owned();
                        json!({"type":"group_profile_updated","message_ids_hex":["ee".repeat(32)]})
                    }
                }
                other => panic!("unexpected group profile request: {other}"),
            };
            response["id"] = req["id"].clone();
            response["marmot_agent_control"] = json!("marmot.agent-control.v2");
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            reader.get_mut().write_all(&bytes).await.unwrap();
        }
    })
}

fn mapped_bridge(config: BridgeConfig, observed: &Agent) -> Bridge {
    let mut bridge = Bridge::open(config).unwrap();
    bridge.state.mappings.insert(
        observed.key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            group_name: Some(observed.group_name()),
            group_description: Some(observed.group_description()),
            output_initialised: true,
            last_agent_status: "idle".into(),
            last_state_change_seq: observed.state_change_seq,
            last_export_at: now(),
            active: true,
            agent_record: Some(observed.clone()),
            ..Mapping::default()
        },
    );
    bridge
}

#[tokio::test]
async fn muted_new_sessions_are_invited_once_without_sending_messages() {
    for edits in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.edit_responses = edits;
        export_script(&config.opencode_bin, "Historical reply", "old");
        herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.reconcile(vec![agent()]).await.unwrap();
        assert!(bridge.state.mappings[&agent().key()].group_id_hex.is_some());
        assert!(!bridge.state.forwarding_enabled);
        assert!(bridge.state.outbox.is_empty());
        bridge.flush().await.unwrap();
        drop(bridge);
        let mut reopened = Bridge::open(config).unwrap();
        reopened.reconcile(vec![agent()]).await.unwrap();
        reopened.flush().await.unwrap();
        let calls = calls.lock().unwrap();
        let invitation = calls
            .iter()
            .find(|call| call["type"] == "group_create_with_admins")
            .unwrap();
        let description = invitation["description"].as_str().unwrap();
        assert!(description.contains(&format!(
            "Hi! This is your chat with {}.",
            agent().group_name()
        )));
        assert!(description.contains("Send /help in any Codr chat to see available commands."));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call["type"] == "group_create_with_admins")
                .count(),
            1
        );
        assert!(
            calls
                .iter()
                .all(|call| call["type"] != "send_final" && call["type"] != "edit_message")
        );
        mdk.abort();
    }
}

#[tokio::test]
async fn introduction_messages_use_the_current_agent_label() {
    for suffix in [None, Some("opencode")] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        export_script(&config.opencode_bin, "Historical reply", "old");
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
        let mut observed = agent();
        observed.terminal_title_stripped = Some("OC | Outdated title".into());
        observed.tab_label = Some("lockr".into());
        observed.workspace_label = Some("bits".into());
        let mut bridge = Bridge::open(config.clone()).unwrap();
        if let Some(suffix) = suffix {
            bridge
                .state
                .pending_chat_agents
                .insert(observed.terminal_id.clone(), suffix.into());
        }
        bridge.reconcile(vec![observed.clone()]).await.unwrap();
        let name = observed.group_name_with_suffix(suffix);
        let greeting = format!(
            "Hi! This is your chat with {name}.\n\nSend /help in any Codr chat to see available commands."
        );
        assert!(bridge.state.outbox.iter().any(|item| item.text == greeting));
        assert!(
            bridge
                .state
                .outbox
                .iter()
                .all(|item| !item.text.contains("OC |"))
        );
        assert!(bridge.state.outbox.iter().any(|item| item.text == greeting));
        drop(bridge);
        let mut bridge = Bridge::open(config).unwrap();
        bridge.reconcile(vec![observed.clone()]).await.unwrap();
        bridge.reconcile(vec![observed]).await.unwrap();
        assert_eq!(
            bridge
                .state
                .outbox
                .iter()
                .filter(|item| item.text == greeting)
                .count(),
            1
        );
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call["type"] == "group_create_with_admins")
                .count(),
            1
        );
        mdk.abort();
    }
}

#[tokio::test]
async fn muted_tab_renames_do_not_wait_for_message_history() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.tab_label = Some("Review".into());
    let profile = Arc::new(Mutex::new(GroupProfile {
        subject: observed.group_name(),
        description: observed.group_description(),
        reject_updates: false,
    }));
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = group_profile_peer(
        &config.wn_agent_socket,
        profile.clone(),
        calls.clone(),
        config.state_path.clone(),
    )
    .await;
    let mut bridge = mapped_bridge(config.clone(), &observed);
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    observed.tab_label = Some("Renamed".into());
    // No transcript executable: metadata must still be updated while delivery is muted.
    bridge.reconcile(vec![observed.clone()]).await.unwrap();
    bridge.reconcile(vec![observed]).await.unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(profile.lock().unwrap().subject, "Renamed");
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["type"] == "group_profile_update")
            .count(),
        1
    );
    assert!(
        calls
            .iter()
            .all(|call| call["type"] != "send_final" && call["type"] != "edit_message")
    );
    assert!(!bridge.state.forwarding_enabled);
    assert!(bridge.state.outbox.is_empty());
    mdk.abort();
}

#[tokio::test]
async fn a_tab_rename_updates_the_group_once() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.tab_label = Some("Review".into());
    let profile = Arc::new(Mutex::new(GroupProfile {
        subject: observed.group_name(),
        description: observed.group_description(),
        reject_updates: false,
    }));
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = group_profile_peer(
        &config.wn_agent_socket,
        profile,
        calls.clone(),
        config.state_path.clone(),
    )
    .await;
    let mut bridge = mapped_bridge(config, &observed);
    let mut renamed = observed.clone();
    renamed.tab_label = Some("Renamed".into());
    bridge.reconcile(vec![renamed.clone()]).await.unwrap();
    bridge.reconcile(vec![renamed]).await.unwrap();
    let updates: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["type"] == "group_profile_update")
        .cloned()
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["name"], "Renamed");
    assert_eq!(
        bridge.state.mappings[&observed.key()].group_name.as_deref(),
        Some("Renamed")
    );
    mdk.abort();
}

#[tokio::test]
async fn fresh_chat_agent_suffix_is_claimed_once_and_survives_restart_and_pane_moves() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    export_script(&config.opencode_bin, "Prior reply", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut source = agent();
    source.tab_label = Some("review".into());
    source.workspace_label = Some("Acme".into());
    let mut fresh = source.clone();
    fresh.terminal_id = "fresh-terminal".into();
    fresh.pane_id = "w1:p2".into();
    fresh.tab_id = "w1:t2".into();
    fresh.agent_session.as_mut().unwrap().value = "ses_fresh".into();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        source.key(),
        Mapping {
            group_id_hex: Some("dd".repeat(16)),
            group_name: Some(source.group_name()),
            group_description: Some(source.group_description()),
            agent_record: Some(source.clone()),
            active: true,
            output_initialised: true,
            final_ids: vec!["old".into()],
            ..Mapping::default()
        },
    );
    bridge
        .state
        .pending_chat_agents
        .insert(fresh.terminal_id.clone(), "opencode".into());
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .reconcile(vec![source.clone(), fresh.clone()])
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&fresh.key()]
            .group_agent_suffix
            .as_deref(),
        Some("opencode")
    );
    assert_eq!(
        bridge.state.mappings[&fresh.key()].group_name.as_deref(),
        Some("review (Acme - opencode)")
    );
    assert!(bridge.state.pending_chat_agents.is_empty());
    assert_eq!(
        bridge.state.mappings[&source.key()].group_name.as_deref(),
        Some("review (Acme)")
    );
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    fresh.terminal_id = "moved-terminal".into();
    fresh.pane_id = "w2:p9".into();
    bridge
        .reconcile(vec![source.clone(), fresh.clone()])
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&fresh.key()].group_name.as_deref(),
        Some("review (Acme - opencode)")
    );
    let calls = calls.lock().unwrap();
    let creates: Vec<_> = calls
        .iter()
        .filter(|c| c["type"] == "group_create_with_admins")
        .collect();
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0]["name"], "review (Acme - opencode)");
    assert!(!calls.iter().any(|c| c["type"] == "group_profile_update"));
    mdk.abort();
}

#[tokio::test]
async fn naming_intent_for_a_different_backend_cannot_label_an_unrelated_chat() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    export_script(&config.opencode_bin, "Prior", "old");
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut observed = agent();
    observed.tab_label = Some("review".into());
    observed.workspace_label = Some("Acme".into());
    let mut bridge = Bridge::open(config).unwrap();
    bridge
        .state
        .pending_chat_agents
        .insert(observed.terminal_id.clone(), "codex".into());
    bridge.reconcile(vec![observed.clone()]).await.unwrap();
    assert!(
        bridge.state.mappings[&observed.key()]
            .group_agent_suffix
            .is_none()
    );
    assert_eq!(
        bridge.state.mappings[&observed.key()].group_name.as_deref(),
        Some("review (Acme)")
    );
    assert_eq!(
        bridge
            .state
            .pending_chat_agents
            .get(&observed.terminal_id)
            .map(String::as_str),
        Some("codex")
    );
    mdk.abort();
}

#[tokio::test]
async fn automatic_and_explicit_renames_keep_the_fresh_agent_suffix() {
    for explicit in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let mut observed = agent();
        observed.tab_label = Some("Review".into());
        observed.workspace_label = Some("Acme".into());
        let profile = Arc::new(Mutex::new(GroupProfile {
            subject: "Review (Acme - opencode)".into(),
            description: observed.group_description(),
            reject_updates: false,
        }));
        let calls = Arc::new(Mutex::new(vec![]));
        let mdk = group_profile_peer(
            &config.wn_agent_socket,
            profile.clone(),
            calls.clone(),
            config.state_path.clone(),
        )
        .await;
        let mut bridge = mapped_bridge(config.clone(), &observed);
        bridge
            .state
            .mappings
            .get_mut(&observed.key())
            .unwrap()
            .group_agent_suffix = Some("opencode".into());
        bridge
            .state
            .mappings
            .get_mut(&observed.key())
            .unwrap()
            .group_name = Some("Review (Acme - opencode)".into());
        bridge.save().unwrap();
        if explicit {
            profile.lock().unwrap().subject = "Old title".into();
            let herdr = peer_for_agent(
                &config.herdr_socket,
                false,
                Arc::new(Mutex::new(vec![])),
                observed.clone(),
            )
            .await;
            bridge.rename_groups().await.unwrap();
            herdr.abort();
            assert_eq!(profile.lock().unwrap().subject, "Review (Acme - opencode)");
        } else {
            observed.tab_label = Some("Renamed".into());
            bridge.reconcile(vec![observed.clone()]).await.unwrap();
            assert_eq!(profile.lock().unwrap().subject, "Renamed (Acme - opencode)");
        }
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c["type"] == "group_profile_update")
                .count(),
            1
        );
        mdk.abort();
    }
}

#[tokio::test]
async fn a_group_renamed_by_the_user_is_not_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.tab_label = Some("Review".into());
    let profile = Arc::new(Mutex::new(GroupProfile {
        subject: "My project".into(),
        description: observed.group_description(),
        reject_updates: false,
    }));
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = group_profile_peer(
        &config.wn_agent_socket,
        profile,
        calls.clone(),
        config.state_path.clone(),
    )
    .await;
    let mut bridge = mapped_bridge(config, &observed);
    let mut renamed = observed.clone();
    renamed.tab_label = Some("Renamed".into());
    bridge.reconcile(vec![renamed]).await.unwrap();
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|request| request["type"] != "group_profile_update")
    );
    assert!(bridge.state.mappings[&observed.key()].group_name_locked);
    mdk.abort();
}

#[tokio::test]
async fn an_unconfirmed_profile_update_is_not_retried() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let mut observed = agent();
    observed.tab_label = Some("Review".into());
    let profile = Arc::new(Mutex::new(GroupProfile {
        subject: observed.group_name(),
        description: observed.group_description(),
        reject_updates: true,
    }));
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = group_profile_peer(
        &config.wn_agent_socket,
        profile,
        calls.clone(),
        config.state_path.clone(),
    )
    .await;
    let mut bridge = mapped_bridge(config.clone(), &observed);
    let mut renamed = observed.clone();
    renamed.tab_label = Some("Renamed".into());
    bridge.reconcile(vec![renamed.clone()]).await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config).unwrap();
    bridge.reconcile(vec![renamed]).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["type"] == "group_profile_update")
            .count(),
        1
    );
    assert!(bridge.state.mappings[&observed.key()].rename_uncertain);
    mdk.abort();
}

#[tokio::test]
async fn control_chat_is_created_once_and_answers_without_an_agent() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.control_chat = true;
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config).unwrap();
    assert!(bridge.state.mappings.contains_key("control"));

    bridge.reconcile(vec![]).await.unwrap();
    let control = &bridge.state.mappings["control"];
    assert_eq!(
        control.group_id_hex.as_deref(),
        Some("bb".repeat(16).as_str())
    );
    assert!(control.active && !control.creating);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["type"] == "group_create_with_admins")
            .count(),
        1
    );

    let mut plain = inbound(HEX, "control-1");
    plain["message"]["text"] = json!("hello");
    bridge.inbound(&plain).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("control chat") && item.text.contains("/new"))
    );

    let mut status = inbound(HEX, "control-2");
    status["message"]["text"] = json!("status");
    bridge.inbound(&status).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains("mapped session"))
    );

    let mut ambiguous = inbound(HEX, "control-3");
    ambiguous["message"]["text"] = json!("/s");
    bridge.inbound(&ambiguous).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .is_some_and(|item| item.text.contains("Ambiguous command"))
    );

    let mut agent = inbound(HEX, "control-4");
    agent["message"]["text"] = json!("/agent");
    bridge.inbound(&agent).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .is_some_and(|item| item.text.contains("not mapped to a project"))
    );

    let mut new = inbound(HEX, "control-5");
    new["message"]["text"] = json!("new");
    bridge.inbound(&new).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .is_some_and(|item| item.text.contains("workspace"))
    );

    let mut menu = inbound(HEX, "control-6");
    menu["message"]["text"] = json!("n");
    bridge.inbound(&menu).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .is_some_and(|item| item.text.contains("setup menu is open"))
    );

    bridge
        .state
        .mappings
        .get_mut("control")
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .expires_at = 0;
    let mut fresh = inbound(HEX, "control-7");
    fresh["message"]["text"] = json!("new");
    bridge.inbound(&fresh).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .is_some_and(|item| item.text.contains("workspace"))
    );

    bridge.reconcile(vec![]).await.unwrap();
    assert!(bridge.state.mappings["control"].active);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["type"] == "group_create_with_admins")
            .count(),
        1
    );
    mdk.abort();
}

#[tokio::test]
async fn control_chat_does_not_block_forwarding_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.control_chat = true;
    let calls = Arc::new(Mutex::new(vec![]));
    let mdk = peer(&config.wn_agent_socket, true, calls.clone()).await;
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.reconcile(vec![]).await.unwrap();

    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    bridge.reconcile(vec![]).await.unwrap();
    assert!(!bridge.state.forwarding_enabled);

    // A phone message wakes forwarding; the synthetic control chat has no
    // transcript, so it must not be treated as a blocking checkpoint.
    herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
    bridge.reconcile(vec![]).await.unwrap();
    assert!(bridge.state.forwarding_enabled);
    assert!(!bridge.state.forwarding_resume_pending);
    mdk.abort();
}

#[tokio::test]
async fn forwarding_reset_does_not_reannounce_an_unchanged_blocked_context() {
    for editable in [false, true] {
        for delivered in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut config = config(root.path());
            config.edit_responses = editable;
            input_history(
                &config,
                "ses_1",
                &[("prior-input", "Prior task")],
                Some(("prior-reply", "Prior answer")),
            );
            // Keep the fixture slower than the former 20 x 10 ms observation
            // window, as can happen when full-binary CI runs contend for I/O.
            let script = std::fs::read_to_string(&config.opencode_bin).unwrap();
            std::fs::write(
                &config.opencode_bin,
                script.replacen("#!/bin/sh\n", "#!/bin/sh\nsleep 0.3\n", 1),
            )
            .unwrap();
            let mut bridge = Bridge::open(config.clone()).unwrap();
            let mut blocked = agent();
            blocked.agent_status = "blocked".into();
            blocked.state_change_seq = 2;
            let observed = Arc::new(Mutex::new(blocked.clone()));
            let calls = Arc::new(Mutex::new(vec![]));
            let herdr = decision_herdr(
                &config.herdr_socket,
                observed.clone(),
                Arc::new(Mutex::new("Input required".into())),
                calls.clone(),
            )
            .await;
            bridge.state.mappings.insert(
                blocked.key(),
                Mapping {
                    group_id_hex: Some("bb".repeat(16)),
                    output_initialised: true,
                    active: true,
                    agent_record: Some(blocked.clone()),
                    last_agent_status: "idle".into(),
                    last_state_change_seq: 1,
                    ..Mapping::default()
                },
            );
            let announced = |bridge: &Bridge| {
                bridge.state.outbox.iter().any(|item| {
                    item.text.contains("Agent needs input")
                        && item.response.as_ref().is_none_or(|response| {
                            response.sent_text.as_deref() != Some(item.text.as_str())
                        })
                })
            };
            for _ in 0..1000 {
                let _ = bridge.poll_sessions(vec![blocked.clone()]).await;
                if announced(&bridge) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(announced(&bridge));
            if delivered {
                bridge.state.outbox.retain_mut(|item| {
                    if let Some(response) = item.response.as_mut() {
                        response.target = Some("delivered-blocked-notice".into());
                        response.sent_text = Some(item.text.clone());
                        true
                    } else {
                        false
                    }
                });
            }

            herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
            for _ in 0..20 {
                let _ = bridge.poll_sessions(vec![blocked.clone()]).await;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|call| call["method"] != "agent.read"),
                "muted blocks must not consume an announcement"
            );
            herdr_white_noise_bridge::forwarding::set(&config.state_path, true).unwrap();
            for _ in 0..1000 {
                let _ = bridge.poll_sessions(vec![blocked.clone()]).await;
                // Wait for the asynchronous checkpoint and blocked-context read,
                // not an assumed CLI completion time under CI contention.
                if bridge.state.forwarding_enabled
                    && !bridge.state.forwarding_resume_pending
                    && bridge.state.mappings[&blocked.key()].blocked_context_seq
                        == Some(blocked.state_change_seq)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(
                bridge.state.forwarding_enabled && !bridge.state.forwarding_resume_pending,
                "forwarding checkpoint must complete before checking delivery"
            );
            assert_eq!(
                bridge.state.mappings[&blocked.key()].blocked_context_seq,
                Some(blocked.state_change_seq),
                "blocked context must finish refreshing"
            );
            assert!(
                announced(&bridge) != delivered,
                "mute/wake must retry only an undelivered blocked notice"
            );

            bridge.state.outbox.clear();
            blocked.state_change_seq = 3;
            *observed.lock().unwrap() = blocked.clone();
            for _ in 0..1000 {
                let _ = bridge.poll_sessions(vec![blocked.clone()]).await;
                if announced(&bridge) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(announced(&bridge), "a new blocked context is announced");
            herdr.abort();
        }
    }
}
