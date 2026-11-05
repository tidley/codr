use bech32::ToBase32;
use herdr_white_noise_bridge::{BridgeConfig, BridgeState};

#[test]
fn repeated_question_resumes_preserve_cleanup_and_finish_the_original_progress() {
    let mut state = BridgeState::with_mapping("session", "group".into());
    state.begin_response("session", "group");
    let text = state.outbox[0].text.clone();
    let delivery = state.outbox[0].response.as_mut().unwrap();
    delivery.target = Some("original-progress".into());
    delivery.sent_text = Some(text);
    state.outbox[0].text = "> blocked Waiting for input".into();
    for _ in 0..2 {
        state.resume_response("session", "group");
        state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let id = state.mappings["session"].response_id.as_ref().unwrap();
        let fresh = state.outbox.iter().find(|item| &item.id == id).unwrap();
        assert_eq!(
            fresh.response.as_ref().unwrap().supersedes.as_deref(),
            Some("original-progress")
        );
    }
    state.complete_progress("session");
    assert_eq!(
        state
            .outbox
            .iter()
            .filter(|item| item
                .response
                .as_ref()
                .is_some_and(|delivery| delivery.target.as_deref() == Some("original-progress")))
            .count(),
        1,
        "Retired pending edits must not overwrite completion"
    );
    assert!(state.outbox.iter().any(|item| {
        item.text == "> Done."
            && item.response.as_ref().is_some_and(|delivery| {
                delivery.target.as_deref() == Some("original-progress") && delivery.finished
            })
    }));
    assert!(!state.outbox.iter().any(|item| {
        item.text.starts_with("> Working.")
            && item
                .response
                .as_ref()
                .is_some_and(|delivery| !delivery.finished)
    }));
}

#[test]
fn working_notice_refreshes_old_bubbles_but_deduplicates_unsent_notices_and_respects_mute() {
    let mut state = BridgeState::with_mapping("session", "group".into());
    state.begin_response("session", "group");
    state.enqueue_working_notice("group");
    assert_eq!(
        state.outbox.len(),
        1,
        "An unsent working bubble is already fresh"
    );
    state.outbox[0].response.as_mut().unwrap().target = Some("old-bubble".into());
    state.enqueue_working_notice("group");
    assert_eq!(
        state.outbox.len(),
        2,
        "An old edited bubble needs a fresh notice"
    );
    let mut restored: BridgeState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    restored.enqueue_working_notice("group");
    assert_eq!(
        restored.outbox.len(),
        2,
        "Do not duplicate a pending notice after restart"
    );
    restored.forwarding_enabled = false;
    restored.enqueue_working_notice("other-group");
    assert_eq!(
        restored.outbox.len(),
        2,
        "Muted chats do not receive working refreshes"
    );
}

#[test]
fn config_rejects_an_invalid_phone_npub() {
    let text = r#"phone_npub = "not-an-npub"
herdr_socket = "/run/herdr.sock"
wn_agent_socket = "/run/wn-agent.sock"
account_id_hex = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
state_path = "/tmp/state.json"
"#;

    assert!(BridgeConfig::from_toml(text).is_err());
}

#[test]
fn state_round_trip_keeps_the_session_group_mapping() {
    let state = BridgeState::with_mapping("herdr:codex:session-1", "ab".repeat(16));
    let path = std::env::temp_dir().join(format!(
        "herdr-white-noise-bridge-state-{}-{}.json",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));

    state.save(&path).unwrap();
    assert_eq!(BridgeState::load(&path).unwrap(), state);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn source_delivery_requires_all_chunks_and_survives_restart_per_group() {
    let mut state = BridgeState::default();
    state.enqueue_tracked(
        "one",
        &"🦄".repeat(20000),
        Some("sync:request"),
        vec!["source-key".into()],
    );
    assert!(state.outbox.len() > 1);
    assert!(state.source_known("one", "source-key"));
    assert!(!state.source_known("two", "source-key"));
    let first = state.outbox[0].id.clone();
    state.confirm_send(&first);
    assert!(state.delivered_messages.is_empty());
    let mut state: BridgeState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(state.source_known("one", "source-key"));
    while let Some(item) = state.outbox.first() {
        let id = item.id.clone();
        state.confirm_send(&id);
    }
    assert_eq!(state.delivered_messages["one"], vec!["source-key"]);
    assert!(!state.source_known("two", "source-key"));
    let restored: BridgeState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(restored.source_known("one", "source-key"));
}

#[test]
fn legacy_state_and_outbox_do_not_invent_source_delivery_records() {
    let mut state = BridgeState::default();
    state.enqueue("group", "old queued answer");
    let mut json = serde_json::to_value(&state).unwrap();
    json.as_object_mut().unwrap().remove("delivered_messages");
    let mut state: BridgeState = serde_json::from_value(json).unwrap();
    let id = state.outbox[0].id.clone();
    state.confirm_send(&id);
    assert!(state.outbox.is_empty());
    assert!(state.delivered_messages.is_empty());
}

#[test]
fn completed_progress_keeps_tasks_from_current_and_previous_working_labels() {
    for label in ["> Working...", "> Working.", "> working...", "> working"] {
        let mut state = BridgeState::with_mapping("session", "ab".repeat(16));
        state.forwarding_enabled = true;
        state.update_response(
            "session",
            &"ab".repeat(16),
            &format!("{label}\n\n✓ Keep this task"),
            false,
        );
        state.outbox[0].response.as_mut().unwrap().target = Some("sent-progress".into());
        state.complete_progress("session");
        assert_eq!(state.outbox[0].text, "> Done.\n\n✓ Keep this task");
    }
}

#[test]
fn native_transcript_roots_default_for_old_configs_and_reject_relative_overrides() {
    let owner = bech32::encode("npub", [0x11u8; 32].to_base32(), bech32::Variant::Bech32).unwrap();
    let base = format!(
        "phone_npub = {owner:?}\nherdr_socket = '/run/herdr.sock'\nwn_agent_socket = '/run/wn.sock'\naccount_id_hex = '{}'\nstate_path = '/tmp/state.json'\n",
        "aa".repeat(32)
    );
    let config = BridgeConfig::from_toml(&base).unwrap();
    assert!(config.codex_home.is_absolute());
    assert!(config.claude_home.is_absolute());
    assert!(config.opencode_db.is_none());
    assert!(BridgeConfig::from_toml(&(base.clone() + "opencode_db = 'relative.db'\n")).is_err());
    assert!(BridgeConfig::from_toml(&(base + "codex_home = 'relative'\n")).is_err());
}

#[test]
fn launch_root_accepts_an_explicit_absolute_project_directory() {
    let owner = bech32::encode("npub", [0x11u8; 32].to_base32(), bech32::Variant::Bech32).unwrap();
    let base = format!(
        "phone_npub = {owner:?}\nherdr_socket = '/run/herdr.sock'\nwn_agent_socket = '/run/wn.sock'\naccount_id_hex = '{}'\nstate_path = '/tmp/state.json'\n",
        "aa".repeat(32)
    );
    assert!(BridgeConfig::from_toml(&(base.clone() + "launch_root = '/tmp/projects'\n")).is_ok());
    assert_eq!(
        BridgeConfig::from_toml(&base).unwrap().launch_root,
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("code")
    );
    assert!(BridgeConfig::from_toml(&(base.clone() + "launch_root = 'relative'\n")).is_err());
    assert!(BridgeConfig::from_toml(&(base + "launch_root = '/'\n")).is_err());
}

#[test]
fn old_state_has_no_fresh_chat_naming_intent_or_suffix() {
    let state = BridgeState::with_mapping("old", "bb".repeat(16));
    let mut json = serde_json::to_value(state).unwrap();
    json.as_object_mut().unwrap().remove("pending_chat_agents");
    json["mappings"]["old"]
        .as_object_mut()
        .unwrap()
        .remove("group_agent_suffix");
    let state: BridgeState = serde_json::from_value(json).unwrap();
    assert!(state.pending_chat_agents.is_empty());
    assert!(state.mappings["old"].group_agent_suffix.is_none());
}
