use bech32::ToBase32;
use herdr_white_noise_bridge::{BridgeConfig, herdr::Session, output::tasks};
use serde_json::json;

fn config(root: &std::path::Path) -> BridgeConfig {
    serde_json::from_value(json!({
        "phone_npub":bech32::encode("npub",[0x11u8;32].to_base32(),bech32::Variant::Bech32).unwrap(),
        "account_id_hex":"aa".repeat(32), "herdr_socket":root.join("h.sock"),
        "wn_agent_socket":root.join("m.sock"), "state_path":root.join("state.json"),
        "codex_home":root.join("codex"), "claude_home":root.join("claude")
    })).unwrap()
}

#[tokio::test]
async fn codex_tasks_require_success_and_do_not_reuse_the_previous_turn() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let id = "11111111-1111-4111-8111-111111111111";
    let path = config
        .codex_home
        .join("sessions")
        .join(format!("rollout-test-{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let session = Session {
        agent: "codex".into(),
        kind: "id".into(),
        source: "herdr:codex".into(),
        value: id.into(),
    };
    let mut rows = vec![
        json!({"type":"session_meta","payload":{"id":id}}),
        json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn"}}),
        json!({"type":"response_item","payload":{"type":"function_call","name":"update_plan","call_id":"call1","arguments":json!({"plan":[{"step":"Run tests","status":"in_progress"}]}).to_string()}}),
    ];
    let write = |rows: &Vec<serde_json::Value>| {
        std::fs::write(
            &path,
            rows.iter()
                .map(|r| r.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap()
    };
    write(&rows);
    assert!(
        tasks::read_session(&config, &session)
            .await
            .unwrap()
            .is_none()
    );
    rows.push(json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call1","output":"Plan updated"}}));
    write(&rows);
    assert!(
        tasks::read_session(&config, &session)
            .await
            .unwrap()
            .unwrap()
            .text()
            .contains("▶ Run tests")
    );
    rows.push(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"next"}}));
    write(&rows);
    assert!(
        tasks::read_session(&config, &session)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn claude_tasks_follow_main_conversation_and_apply_successful_updates() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let id = "11111111-1111-4111-8111-111111111111";
    let path = config
        .claude_home
        .join("projects/project")
        .join(format!("{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let session = Session {
        agent: "claude".into(),
        kind: "id".into(),
        source: "herdr:claude".into(),
        value: id.into(),
    };
    let rows = [
        json!({"uuid":"u","type":"user","sessionId":id,"message":{"role":"user","content":"Work"}}),
        json!({"uuid":"a","parentUuid":"u","type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"TaskCreate","input":{"subject":"Run checks"}}]}}),
        json!({"uuid":"r","parentUuid":"a","type":"user","sessionId":id,"toolUseResult":{"task":{"id":"1","subject":"Run checks"}},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"Task #1 created successfully: Run checks"}]}}),
        json!({"uuid":"a2","parentUuid":"r","type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"tool_use","id":"c2","name":"TaskUpdate","input":{"taskId":"1","status":"in_progress"}}]}}),
        json!({"uuid":"r2","parentUuid":"a2","type":"user","sessionId":id,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c2","content":"Updated task #1 status"}]}}),
        json!({"uuid":"side","parentUuid":"r2","type":"assistant","sessionId":id,"isSidechain":true,"message":{"role":"assistant","content":[{"type":"tool_use","id":"evil","name":"TodoWrite","input":{"todos":[{"content":"Private side task","status":"pending"}]}}]}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(|r| r.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let text = tasks::read_session(&config, &session)
        .await
        .unwrap()
        .unwrap()
        .text();
    assert!(text.contains("▶ Run checks"));
    assert!(!text.contains("Private side task"));
}

#[tokio::test]
async fn codex_turn_identity_is_available_before_the_final_record() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let id = "11111111-1111-4111-8111-111111111111";
    let path = config
        .codex_home
        .join("sessions")
        .join(format!("rollout-test-{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let session = Session {
        agent: "codex".into(),
        kind: "id".into(),
        source: "herdr:codex".into(),
        value: id.into(),
    };
    let prefix = format!(
        "{}\n{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":id}}),
        json!({"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"user_message","message":"Work"}}),
        json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-a"}})
    );
    std::fs::write(&path, &prefix).unwrap();
    let history = herdr_white_noise_bridge::output::read_history(&config, &session)
        .await
        .unwrap();
    assert!(history.finals.is_empty());
    assert_eq!(history.user_inputs.len(), 1);
    assert_eq!(
        history.user_inputs[0].fingerprint,
        herdr_white_noise_bridge::activity::UserInput::new(String::new(), "Work").fingerprint
    );
    assert_eq!(history.latest_turn.as_deref(), Some("codex:turn-a"));
    std::fs::write(&path,format!("{prefix}{}\n{}\n",json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-a","last_agent_message":"Late final"}}),json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-b"}}))).unwrap();
    let history = herdr_white_noise_bridge::output::read_history(&config, &session)
        .await
        .unwrap();
    assert_eq!(history.finals[0].turn.as_deref(), Some("codex:turn-a"));
    assert_eq!(history.latest_turn.as_deref(), Some("codex:turn-b"));
    assert_eq!(history.turns, ["codex:turn-a", "codex:turn-b"]);
}

#[tokio::test]
async fn claude_turn_identity_uses_the_main_user_prompt_before_the_final() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let id = "11111111-1111-4111-8111-111111111111";
    let path = config
        .claude_home
        .join("projects/project")
        .join(format!("{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let session = Session {
        agent: "claude".into(),
        kind: "id".into(),
        source: "herdr:claude".into(),
        value: id.into(),
    };
    let prefix = format!(
        "{}\n",
        json!({"uuid":"u1","type":"user","sessionId":id,"message":{"role":"user","content":"Work"}})
    );
    std::fs::write(&path, &prefix).unwrap();
    let history = herdr_white_noise_bridge::output::read_history(&config, &session)
        .await
        .unwrap();
    assert!(history.finals.is_empty());
    assert_eq!(history.latest_turn.as_deref(), Some("claude:u1"));
    assert_eq!(history.user_inputs.len(), 1);
    assert_eq!(history.user_inputs[0].id, "claude:u1");
    assert_eq!(
        history.user_inputs[0].fingerprint,
        herdr_white_noise_bridge::activity::UserInput::new(String::new(), "Work").fingerprint
    );
    std::fs::write(&path,format!("{prefix}{}\n{}\n",json!({"uuid":"a1","parentUuid":"u1","type":"assistant","sessionId":id,"message":{"id":"api","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Late final"}]}}),json!({"uuid":"u2","parentUuid":"a1","type":"user","sessionId":id,"message":{"role":"user","content":"Next request"}}))).unwrap();
    let history = herdr_white_noise_bridge::output::read_history(&config, &session)
        .await
        .unwrap();
    assert_eq!(history.finals[0].turn.as_deref(), Some("claude:u1"));
    assert_eq!(history.latest_turn.as_deref(), Some("claude:u2"));
    assert_eq!(history.turns, ["claude:u1", "claude:u2"]);
}

#[tokio::test]
async fn claude_background_task_notifications_stay_in_the_phone_turn() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let id = "11111111-1111-4111-8111-111111111111";
    let path = config
        .claude_home
        .join("projects/project")
        .join(format!("{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let session = Session {
        agent: "claude".into(),
        kind: "id".into(),
        source: "herdr:claude".into(),
        value: id.into(),
    };
    let rows = [
        json!({"uuid":"u1","type":"user","sessionId":id,"origin":{"kind":"human"},"promptSource":"typed","message":{"role":"user","content":"Phone request"}}),
        json!({"uuid":"a1","parentUuid":"u1","type":"assistant","sessionId":id,"message":{"id":"api-1","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"First reply"}]}}),
        json!({"uuid":"n1","parentUuid":"a1","type":"user","sessionId":id,"origin":{"kind":"task-notification"},"promptSource":"system","message":{"role":"user","content":"<task-notification>done</task-notification>"}}),
        json!({"uuid":"a2","parentUuid":"n1","type":"assistant","sessionId":id,"message":{"id":"api-2","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Background result"}]}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let history = herdr_white_noise_bridge::output::read_history(&config, &session)
        .await
        .unwrap();
    assert_eq!(
        history
            .user_inputs
            .iter()
            .map(|input| input.id.as_str())
            .collect::<Vec<_>>(),
        ["claude:u1"]
    );
    assert_eq!(history.turns, ["claude:u1"]);
    assert_eq!(
        history
            .finals
            .iter()
            .map(|reply| (reply.text.as_str(), reply.turn.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("First reply", Some("claude:u1")),
            ("Background result", Some("claude:u1"))
        ]
    );
}
