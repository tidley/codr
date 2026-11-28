//! Accepted access-spec integration tests. All authority comes from the private
//! MDK context fixture, never from the inbound envelope or raw @alias text.
use super::*;
use herdr_white_noise_bridge::porter;

#[path = "room_controls.rs"]
mod owner_controls;

#[tokio::test]
async fn desktop_native_prepare_mirrors_before_normal_admission_without_second_post() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    // Native Enter can make the terminal look working before HTTP admission.
    fixture.agents.lock().unwrap()[0].agent_status = "working".into();
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),
        "terminal_id":agent().terminal_id,"native_message_id":"msg_desktop_123",
        "text":"Normal desktop input"});
    let receipt = control(&mut bridge, "room.desktop_prepare", request.clone())
        .await
        .unwrap();
    assert_eq!(receipt["native_admission"], "prepared");
    assert_eq!(receipt["native_message_id"], "msg_desktop_123");
    assert!(
        fixture.posts.lock().unwrap().is_empty(),
        "The hook lets OpenCode admit the original input"
    );
    assert_eq!(
        control(&mut bridge, "room.desktop_prepare", request.clone())
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "send_final")
            .count(),
        1
    );
    let registration: Value = serde_json::from_slice(
        &std::fs::read(fixture.config.opencode_service_file.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    reqwest::Client::new()
        .post(format!(
            "{}/api/session/ses_1/prompt",
            registration["url"].as_str().unwrap()
        ))
        .json(&json!({"id":"msg_desktop_123","text":"Normal desktop input"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .push(json!({"id":"msg_desktop_123","type":"user","text":"Normal desktop input"}));
    let mut reply = final_message("normal-desktop-answer", "Normal desktop reply");
    reply["parentID"] = json!("msg_desktop_123");
    fixture.agents.lock().unwrap()[0].agent_status = "idle".into();
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .push(reply);
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_none()
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "**@builder**\nNormal desktop reply")
    );
    control(&mut bridge, "room.stop", json!({})).await.unwrap();
    assert!(
        control(&mut bridge, "room.desktop_prepare", request)
            .await
            .is_err(),
        "Prepared receipts cannot bypass a later stop"
    );
}

#[tokio::test]
async fn desktop_native_prepare_rejects_changed_native_id_and_held_room() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),
        "terminal_id":agent().terminal_id,"native_message_id":"msg_desktop_456","text":"One input"});
    control(&mut bridge, "room.desktop_prepare", request.clone())
        .await
        .unwrap();
    let mut changed = request.clone();
    changed["native_message_id"] = json!("msg_desktop_other");
    assert!(
        control(&mut bridge, "room.desktop_prepare", changed)
            .await
            .is_err()
    );
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .room_quarantined = Some("native_input_outside_room_stream".into());
    assert!(
        control(&mut bridge, "room.desktop_prepare", request)
            .await
            .is_err()
    );
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn desktop_relay_publishes_as_codr_and_admits_once_across_restart() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    let request = json!({"request_id":"e27d199c-f268-4161-8cc1-dd5d5b4b1f04",
        "terminal_id":agent().terminal_id,"text":"How is the weather?"});
    let result = control(&mut bridge, "room.desktop_send", request.clone())
        .await
        .unwrap();
    assert_eq!(result["native_admission"], "accepted");
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert_eq!(
        fixture.posts.lock().unwrap()[0].1["text"],
        "How is the weather?"
    );
    let mirrors: Vec<_> = fixture
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| {
            c["type"] == "send_final"
                && c["text"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("Desktop via Codr\n\n"))
        })
        .cloned()
        .collect();
    assert_eq!(mirrors.len(), 1);
    assert_eq!(mirrors[0]["account_id_hex"], fixture.config.account_id_hex);
    assert!(
        mirrors[0]["addressed_accounts"].is_null(),
        "No forged recipient authority"
    );
    assert_eq!(
        bridge.state.mappings[&agent().key()]
            .shared_admissions
            .len(),
        1
    );
    drop(bridge);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    assert_eq!(
        control(&mut bridge, "room.desktop_send", request.clone())
            .await
            .unwrap(),
        result
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    let mut changed = request;
    changed["text"] = json!("Different instruction");
    assert!(
        control(&mut bridge, "room.desktop_send", changed)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn desktop_relay_requires_owner_grant_live_identity_and_unheld_roster() {
    for blocked in [
        "grant",
        "stop",
        "quarantine",
        "roster",
        "terminal",
        "private",
    ] {
        let (fixture, mut bridge) = room().await;
        classify(&mut bridge).await;
        if blocked != "grant" {
            grant(&mut bridge, HEX, "human").await;
        }
        if blocked == "stop" {
            control(&mut bridge, "room.stop", json!({})).await.unwrap();
        }
        if blocked == "quarantine" {
            bridge
                .state
                .mappings
                .get_mut(&agent().key())
                .unwrap()
                .room_quarantined = Some("native_input_outside_room_stream".into());
        }
        if blocked == "roster" {
            fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
        }
        if blocked == "private" {
            fixture
                .histories
                .lock()
                .unwrap()
                .get_mut("ses_1")
                .unwrap()
                .push(json!({"id":"private","type":"user","text":"private secret"}));
        }
        let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),
            "terminal_id":if blocked == "terminal" {"wrong".to_owned()} else {agent().terminal_id},"text":"Do not leak or invoke"});
        assert!(
            control(&mut bridge, "room.desktop_send", request)
                .await
                .is_err(),
            "{blocked}"
        );
        assert!(fixture.posts.lock().unwrap().is_empty(), "{blocked}");
        assert!(
            !fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c["type"] == "send_final"),
            "{blocked}"
        );
    }
}

#[tokio::test]
async fn desktop_relay_reply_uses_existing_correlated_room_output() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    control(
        &mut bridge,
        "room.desktop_send",
        json!({"request_id":uuid::Uuid::new_v4().to_string(),
        "terminal_id":agent().terminal_id,"text":"Desktop work"}),
    )
    .await
    .unwrap();
    let mut reply = final_message("desktop-answer", "Desktop answer");
    reply["parentID"] = fixture.posts.lock().unwrap()[0].1["id"].clone();
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .push(reply);
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(
        bridge.state.outbox.iter().any(
            |item| item.group == "bb".repeat(16) && item.text == "**@builder**\nDesktop answer"
        )
    );
    bridge.flush().await.unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge.flush().await.unwrap();
    assert_eq!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["type"] == "send_final"
                && call["text"] == "**@builder**\nDesktop answer")
            .count(),
        1
    );
}

#[tokio::test]
async fn desktop_relay_native_uncertainty_does_not_replay_publication_or_prompt() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    fixture.lose.lock().unwrap().push("ses_1".into());
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),
        "terminal_id":agent().terminal_id,"text":"One attempt only"});
    let result = control(&mut bridge, "room.desktop_send", request.clone())
        .await
        .unwrap();
    assert_eq!(result["native_admission"], "uncertain");
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    let count = fixture.calls.lock().unwrap().len();
    assert_eq!(
        control(&mut bridge, "room.desktop_send", request)
            .await
            .unwrap(),
        result
    );
    assert_eq!(fixture.calls.lock().unwrap().len(), count);
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn desktop_relay_failed_publish_or_late_roster_change_never_admits_or_replays() {
    for failure in ["publish", "roster"] {
        let (fixture, mut bridge) = room().await;
        classify(&mut bridge).await;
        grant(&mut bridge, HEX, "human").await;
        if failure == "publish" {
            fixture.info.lock().unwrap()["desktop_send_denied"] = json!(true);
        } else {
            fixture.info.lock().unwrap()["desktop_members_after_send"] =
                json!([HEX, "aa".repeat(32)]);
        }
        let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),
            "terminal_id":agent().terminal_id,"text":"Never retry an uncertain relay"});
        assert!(
            control(&mut bridge, "room.desktop_send", request.clone())
                .await
                .is_err(),
            "{failure}"
        );
        assert!(fixture.posts.lock().unwrap().is_empty());
        let count = fixture.calls.lock().unwrap().len();
        drop(bridge);
        let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
        assert!(
            control(&mut bridge, "room.desktop_send", request)
                .await
                .is_err()
        );
        assert_eq!(fixture.calls.lock().unwrap().len(), count);
    }
}

pub(super) async fn room() -> (Fixture, Bridge) {
    room_backend(false).await
}

async fn room_backend(core: bool) -> (Fixture, Bridge) {
    let fixture = Fixture::new().await;
    if core {
        for (index, agent) in fixture.agents.lock().unwrap().iter_mut().enumerate() {
            agent.pane_id = format!("t{}", index + 1);
            let source = format!(
                "hc:{}",
                json!([
                    "fixture-core",
                    agent.pane_id,
                    1234,
                    "/fixture/opencode",
                    "/fixture/project"
                ])
            );
            agent.terminal_id = source.clone();
            agent.agent_session.as_mut().unwrap().source = source;
        }
        let discovered = herdr::snapshot(&Socket {
            path: fixture.config.herdr_socket.clone(),
        })
        .await
        .unwrap();
        *fixture.agents.lock().unwrap() = discovered;
    }
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.rows.lock().unwrap().clear();
    fixture.info.lock().unwrap()["room_members"] =
        json!([HEX, "22".repeat(32), "33".repeat(32), "aa".repeat(32)]);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge
        .attach_room(&fixture.snapshot()[0].key(), &"bb".repeat(16), "builder")
        .await
        .unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert_eq!(
        bridge.state.mappings.len(),
        1,
        "Dedicated room transport must not auto-create private groups for unrelated sessions"
    );
    (fixture, bridge)
}

async fn control(bridge: &mut Bridge, method: &str, mut fields: Value) -> anyhow::Result<Value> {
    fields["version"] = json!(1);
    fields["bridge_account"] = json!(bridge.config.account_id_hex);
    fields["method"] = json!(method);
    fields["session_key"] = json!(
        bridge
            .state
            .mappings
            .iter()
            .find(|(_, m)| m.room.is_some())
            .map(|(key, _)| key.clone())
            .unwrap_or_else(|| agent().key())
    );
    porter::handle(bridge, fields).await
}

#[tokio::test]
async fn core_room_preserves_native_hook_attribution_defaults_privacy_and_no_duplicate_admission() {
    let (fixture, mut bridge) = room_backend(true).await;
    let retained = fixture.snapshot()[0].clone();
    let key = retained.key();
    classify(&mut bridge).await;
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"terminal_id":retained.terminal_id,
        "native_message_id":"msg_desktop_core","text":"Normal Core desktop input"});
    assert!(
        control(&mut bridge, "room.desktop_prepare", request.clone())
            .await
            .is_err(),
        "No implicit owner invocation grant"
    );
    grant(&mut bridge, HEX, "human").await;
    bridge
        .inbound(&event(&fixture, 4100, HEX, "ordinary conversation", false))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    fixture.agents.lock().unwrap()[0].agent_status = "unknown".into();
    bridge
        .inbound(&event(
            &fixture,
            4099,
            HEX,
            "unknown status must not invoke",
            true,
        ))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        control(&mut bridge, "room.desktop_prepare", request.clone())
            .await
            .is_err()
    );
    fixture.agents.lock().unwrap()[0].agent_status = "idle".into();
    let receipt = control(&mut bridge, "room.desktop_prepare", request.clone())
        .await
        .unwrap();
    assert_eq!(receipt["native_admission"], "prepared");
    assert_eq!(
        control(&mut bridge, "room.desktop_prepare", request)
            .await
            .unwrap(),
        receipt
    );
    assert!(
        fixture.posts.lock().unwrap().is_empty(),
        "Normal Enter is admitted by OpenCode, not a second Codr POST"
    );
    assert_eq!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "send_final"
                && c["text"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("Desktop via Codr\n\n")))
            .count(),
        1
    );
    {
        let mut histories = fixture.histories.lock().unwrap();
        let history = histories.get_mut("ses_1").unwrap();
        history.push(
            json!({"id":"msg_desktop_core","type":"user","text":"Normal Core desktop input"}),
        );
        let mut reply = final_message("core-final", "Core native reply");
        reply["parentID"] = json!("msg_desktop_core");
        history.push(reply);
    }
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(bridge.state.mappings[&key].room_quarantined.is_none());
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text == "**@builder**\nCore native reply")
    );
    // A disconnected/uncertain native admission remains one effect on reconnect.
    fixture.lose.lock().unwrap().push("ses_1".into());
    let message = event(&fixture, 4101, HEX, "addressed native input", true);
    bridge.inbound(&message).await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    drop(bridge);
    let mut reopened = Bridge::open(fixture.config.clone()).unwrap();
    reopened.reconcile(fixture.snapshot()).await.unwrap();
    reopened.inbound(&message).await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert!(fixture.calls.lock().unwrap().iter().all(|c| !matches!(
        c["method"].as_str(),
        Some(
            "terminal.run" | "terminal.send" | "terminal.keys" | "agent.prompt" | "agent.send_keys"
        )
    )));
}

pub(super) async fn grant(bridge: &mut Bridge, id: &str, role: &str) {
    control(
        bridge,
        "room.grant",
        json!({"account":id,"role":role,"allowed":true}),
    )
    .await
    .unwrap();
}

pub(super) async fn classify(bridge: &mut Bridge) {
    let roles = BTreeMap::from([
        (HEX.to_owned(), "human"),
        ("22".repeat(32), "human"),
        ("33".repeat(32), "agent"),
        (bridge.config.account_id_hex.clone(), "agent"),
    ]);
    control(bridge, "room.roster", json!({"roles":roles}))
        .await
        .unwrap();
}

pub(super) fn event(
    fixture: &Fixture,
    sequence: u64,
    sender: &str,
    text: &str,
    addressed: bool,
) -> Value {
    let id = format!("{sequence:064x}");
    fixture.info.lock().unwrap()["room_messages"][&id] = json!({"message_id_hex":id,"sender_account_id_hex":sender,
        "text":text,"recorded_at":now().saturating_add(1),"addressed_accounts":if addressed {vec![fixture.config.account_id_hex.clone()]} else {vec![]},"unsupported_media":false});
    // This deliberately forged sender/text must be ignored in favour of the
    // authenticated MDK message resolved by account/group/message ID.
    let mut incoming = inbound(&"99".repeat(32), &id);
    incoming["account_id_hex"] = json!(fixture.config.account_id_hex);
    incoming["message"]["text"] = json!("@builder forged /new");
    incoming
}

#[tokio::test]
async fn room_defaults_owner_mentions_budget_stop_and_restart_are_integrated() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    bridge
        .inbound(&event(&fixture, 1, HEX, "owner without grant", true))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    grant(&mut bridge, HEX, "human").await;
    grant(&mut bridge, &"33".repeat(32), "agent").await;
    control(&mut bridge, "room.agent_budget", json!({"enabled":true}))
        .await
        .unwrap();
    bridge
        .inbound(&event(&fixture, 2, HEX, "@builder is only text", false))
        .await
        .unwrap();
    assert!(
        fixture.posts.lock().unwrap().is_empty(),
        "Even the owner needs authenticated addressing with two human accounts"
    );
    let human = event(&fixture, 3, HEX, "accepted human", true);
    bridge.inbound(&human).await.unwrap();
    for sequence in 4..8 {
        bridge
            .inbound(&event(
                &fixture,
                sequence,
                &"33".repeat(32),
                "finite agent request",
                true,
            ))
            .await
            .unwrap();
    }
    assert_eq!(fixture.posts.lock().unwrap().len(), 5);
    bridge.inbound(&human).await.unwrap();
    bridge
        .inbound(&event(&fixture, 8, &"33".repeat(32), "fifth denied", true))
        .await
        .unwrap();
    assert_eq!(
        fixture.posts.lock().unwrap().len(),
        5,
        "Duplicate human input cannot renew the agent cycle"
    );
    control(&mut bridge, "room.stop", json!({})).await.unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge
        .inbound(&event(&fixture, 9, HEX, "stopped human", true))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 5);
    control(&mut bridge, "room.resume", json!({}))
        .await
        .unwrap();
    bridge
        .inbound(&event(
            &fixture,
            10,
            &"33".repeat(32),
            "resume cannot renew budget",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 5);
    bridge
        .inbound(&event(&fixture, 11, HEX, "new human cycle", true))
        .await
        .unwrap();
    bridge
        .inbound(&event(
            &fixture,
            12,
            &"33".repeat(32),
            "new finite agent",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 7);
    assert!(
        fixture
            .posts
            .lock()
            .unwrap()
            .iter()
            .all(|(session, _)| session == "ses_1")
    );
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt"
                || call["type"] == "group_create_with_admins")
    );
}

#[tokio::test]
async fn room_roster_revocation_recovery_and_private_commands_fail_closed() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, &"22".repeat(32), "human").await;
    let denied = event(
        &fixture,
        20,
        &"22".repeat(32),
        "@builder no signed address",
        false,
    );
    bridge.inbound(&denied).await.unwrap();
    // Recovered text cannot acquire addressing by virtue of an alias or a new
    // client envelope. Even an updated fixture row cannot replay consumed input.
    let replay = event(&fixture, 20, &"22".repeat(32), "now addressed", true);
    bridge.inbound(&replay).await.unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    for (sequence, text) in [
        (21, "/new"),
        (22, "/last"),
        (23, "/allow abc"),
        (24, "/sync on"),
    ] {
        bridge
            .inbound(&event(&fixture, sequence, &"22".repeat(32), text, true))
            .await
            .unwrap();
    }
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(control(&mut bridge, "history", json!({})).await.is_err());
    assert!(
        control(&mut bridge, "enqueue", json!({"text":"outside room"}))
            .await
            .is_err()
    );
    fixture.info.lock().unwrap()["room_members"] = json!([
        HEX,
        "22".repeat(32),
        "33".repeat(32),
        "aa".repeat(32),
        "55".repeat(32)
    ]);
    bridge
        .inbound(&event(
            &fixture,
            25,
            &"22".repeat(32),
            "roster changed",
            true,
        ))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        control(&mut bridge, "room.roster", json!({"roles":{}}))
            .await
            .is_err()
    );
    fixture.info.lock().unwrap()["room_members"] =
        json!([HEX, "22".repeat(32), "33".repeat(32), "aa".repeat(32)]);
    control(
        &mut bridge,
        "room.grant",
        json!({"account":"22".repeat(32),"role":"human","allowed":false}),
    )
    .await
    .unwrap();
    bridge
        .inbound(&event(&fixture, 26, &"22".repeat(32), "revoked", true))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_unexpected_native_input_is_durably_quarantined_before_output() {
    let (fixture, bridge) = room().await;
    drop(bridge);
    fixture.histories.lock().unwrap().get_mut("ses_1").unwrap().extend([
        json!({"type":"user","id":"msg_outside","text":"private native input","time":{"created":1}}),
        final_message("outside-answer", "PRIVATE OUTSIDE ANSWER"),
    ]);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .as_deref(),
        Some("native_input_outside_room_stream")
    );
    bridge.flush().await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    drop(bridge);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "send_final")
    );
}

#[tokio::test]
async fn human_host_uses_external_account_sends_without_impersonation_or_uncertain_replay() {
    let fixture = Fixture::new().await;
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    let register = json!({"version":1,"method":"human.register","bridge_account":"aa".repeat(32),
        "request_id":uuid::Uuid::new_v4(),"human_account":"22".repeat(32),"signer_socket":"/private/fixture-only.sock"});
    porter::handle(&mut bridge, register).await.unwrap();
    let send = json!({"version":1,"method":"human.send","bridge_account":"aa".repeat(32),
        "request_id":uuid::Uuid::new_v4(),"human_account":"22".repeat(32),"group":"bb".repeat(16),
        "text":"human authored","addressed_accounts":["aa".repeat(32)]});
    porter::handle(&mut bridge, send.clone()).await.unwrap();
    porter::handle(&mut bridge, send).await.unwrap();
    let sends: Vec<_> = fixture
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call["type"] == "human_account_send")
        .cloned()
        .collect();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["account_id_hex"], "22".repeat(32));
    assert_eq!(sends[0]["addressed_accounts"], json!(["aa".repeat(32)]));
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "send_final")
    );
    fixture.info.lock().unwrap()["human_denied"] = json!(true);
    let denied = json!({"version":1,"method":"human.send","bridge_account":"aa".repeat(32),
        "request_id":uuid::Uuid::new_v4(),"human_account":"22".repeat(32),"group":"bb".repeat(16),
        "text":"not confirmed","addressed_accounts":[]});
    assert!(porter::handle(&mut bridge, denied.clone()).await.is_err());
    drop(bridge);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    fixture.info.lock().unwrap()["human_denied"] = json!(false);
    assert!(porter::handle(&mut bridge, denied).await.is_err());
    assert_eq!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["type"] == "human_account_send")
            .count(),
        2,
        "Refusal/uncertainty survives restart without another signer or send attempt"
    );
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_legacy_alias_binding_cannot_bypass_authenticated_access_policy() {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    bridge
        .inbound(&message(
            HEX,
            "legacy-owner",
            "@builder run without room policy",
        ))
        .await
        .unwrap();
    assert!(
        fixture.posts.lock().unwrap().is_empty(),
        "Legacy aliases must not retain the old implicit owner grant"
    );
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
}

#[tokio::test]
async fn room_changed_native_text_under_accepted_id_quarantines_output() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    bridge
        .inbound(&event(&fixture, 100, HEX, "room input", true))
        .await
        .unwrap();
    let id = fixture.posts.lock().unwrap()[0].1["id"].clone();
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .extend([
            json!({"type":"user","id":id,"text":"CHANGED PRIVATE INPUT","time":{"created":1}}),
            final_message("changed-answer", "PRIVATE ANSWER"),
        ]);
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    bridge.flush().await.unwrap();
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "send_final")
    );
}

#[tokio::test]
async fn room_transport_refuses_additional_bindings_and_foreign_group_commands() {
    let (fixture, mut bridge) = room().await;
    let mut foreign = message(HEX, "foreign", "/new");
    foreign["group_id_hex"] = json!("dd".repeat(16));
    bridge.inbound(&foreign).await.unwrap();
    assert!(
        bridge
            .attach_chat(&second_agent().key(), &"dd".repeat(16), "default")
            .await
            .is_err()
    );
    assert_eq!(bridge.state.mappings.len(), 1);
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_uncertain_native_admission_is_reconciled_without_replay() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    fixture.lose.lock().unwrap().push("ses_1".into());
    let input = event(&fixture, 110, HEX, "uncertain room prompt", true);
    bridge.inbound(&input).await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert_eq!(bridge.state.mappings[&agent().key()].submissions.len(), 1);
    drop(bridge);
    fixture.lose.lock().unwrap().clear();
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge.inbound(&input).await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert!(bridge.state.mappings[&agent().key()].submissions.is_empty());
}

#[tokio::test]
async fn room_failed_reader_preserves_input_for_safe_authenticated_recovery() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    fixture.failed_reads.lock().unwrap().push("ses_1".into());
    let input = event(&fixture, 120, HEX, "read before dispatch", true);
    assert!(bridge.inbound(&input).await.is_err());
    assert!(fixture.posts.lock().unwrap().is_empty());
    fixture.failed_reads.lock().unwrap().clear();
    bridge.inbound(&input).await.unwrap();
    bridge.inbound(&input).await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn room_shared_reply_is_correlated_labelled_and_never_answers_native_forms() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    bridge
        .inbound(&event(&fixture, 130, HEX, "room work", true))
        .await
        .unwrap();
    let mut reply = final_message("room-answer", "Shared room answer");
    reply["parentID"] = fixture.posts.lock().unwrap()[0].1["id"].clone();
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .push(reply);
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(bridge.state.outbox.iter().any(
        |item| item.group == "bb".repeat(16) && item.text == "**@builder**\nShared room answer"
    ));
    fixture
        .forms
        .lock()
        .unwrap()
        .insert("ses_1".into(), json!({"data":[]}));
    fixture.permissions.lock().unwrap().insert(
        "ses_1".into(),
        json!({"data":[{
        "id":"per_room","sessionID":"ses_1","action":"shell","resources":["git status"],
        "source":{"type":"tool","messageID":"msg_room","id":"call_room"},"metadata":{}}]}),
    );
    fixture.agents.lock().unwrap()[0].agent_status = "blocked".into();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge
        .inbound(&event(&fixture, 131, HEX, "1", true))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert!(
        bridge.state.mappings[&agent().key()]
            .permission_attempts
            .is_empty()
    );
}

#[tokio::test]
async fn room_native_terminal_change_quarantines_without_private_mirroring() {
    let (fixture, mut bridge) = room().await;
    fixture.agents.lock().unwrap()[0].terminal_id = "replacement".into();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    assert_eq!(bridge.state.mappings.len(), 1);
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_live_identity_change_between_snapshot_and_dispatch_quarantines() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    // The cached authoritative snapshot still has the old terminal. The fresh
    // agent.get response must quarantine, not merely return a transient error.
    fixture.agents.lock().unwrap()[0].terminal_id = "replaced-before-post".into();
    let _ = bridge
        .inbound(&event(&fixture, 140, HEX, "identity race", true))
        .await;
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_timeline_recovery_retains_authenticated_human_and_agent_addressing() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    grant(&mut bridge, &"33".repeat(32), "agent").await;
    control(&mut bridge, "room.agent_budget", json!({"enabled":true}))
        .await
        .unwrap();
    for (sequence, sender, addressed) in [
        (150, HEX.to_owned(), false),
        (151, HEX.to_owned(), true),
        (152, "33".repeat(32), true),
    ] {
        let _ = event(&fixture, sequence, &sender, "@builder recover", addressed);
        fixture
            .rows
            .lock()
            .unwrap()
            .push(json!({"message_id_hex":format!("{sequence:064x}"),
            "recorded_at":now().saturating_add(1),"kind":9,"availability":"available",
            "sender":{"account_id_hex":sender},"text":"@builder recover"}));
    }
    bridge.recover_inbound().await.unwrap();
    assert_eq!(
        fixture.posts.lock().unwrap().len(),
        2,
        "Only authenticated addressed rows may invoke"
    );
    drop(bridge);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge.recover_inbound().await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn room_attachment_never_converts_existing_private_history_or_bindings() {
    let fixture = Fixture::new().await;
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    assert!(
        bridge
            .attach_room(&agent().key(), &"bb".repeat(16), "builder")
            .await
            .is_err(),
        "Even old assistant history makes a native conversation ineligible"
    );
    assert!(bridge.state.mappings.is_empty());
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    bridge
        .state
        .mappings
        .insert("retained-private".into(), Mapping::default());
    assert!(
        bridge
            .attach_room(&agent().key(), &"bb".repeat(16), "builder")
            .await
            .is_err()
    );
    assert_eq!(bridge.state.mappings.len(), 1);
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_attachment_baseline_ids_cannot_replay_with_a_future_transport_clock() {
    let fixture = Fixture::new().await;
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.info.lock().unwrap()["room_members"] =
        json!([HEX, "22".repeat(32), "33".repeat(32), "aa".repeat(32)]);
    let old = event(&fixture, 160, HEX, "old input with future clock", true);
    fixture
        .rows
        .lock()
        .unwrap()
        .push(json!({"message_id_hex":format!("{:064x}",160),
        "recorded_at":now().saturating_add(1),"kind":9,"availability":"available",
        "sender":{"account_id_hex":HEX},"text":"old input with future clock"}));
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge
        .attach_room(&agent().key(), &"bb".repeat(16), "builder")
        .await
        .unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    bridge.inbound(&old).await.unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn room_observed_roster_change_stays_invalid_after_return_and_restart() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    grant(&mut bridge, &"33".repeat(32), "agent").await;
    control(&mut bridge, "room.agent_budget", json!({"enabled":true}))
        .await
        .unwrap();
    bridge
        .inbound(&event(&fixture, 170, HEX, "start cycle", true))
        .await
        .unwrap();
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "33".repeat(32), "aa".repeat(32)]);
    bridge
        .inbound(&event(&fixture, 171, HEX, "observe removal", true))
        .await
        .unwrap();
    drop(bridge);
    fixture.info.lock().unwrap()["room_members"] =
        json!([HEX, "22".repeat(32), "33".repeat(32), "aa".repeat(32)]);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge
        .inbound(&event(
            &fixture,
            172,
            HEX,
            "returned roster is not reconciliation",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    classify(&mut bridge).await;
    bridge
        .inbound(&event(
            &fixture,
            173,
            &"33".repeat(32),
            "reconciliation cannot renew budget",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    bridge
        .inbound(&event(&fixture, 174, HEX, "new accepted human", true))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn room_two_configurations_cannot_attach_the_same_empty_native_conversation() {
    let fixture = Fixture::new().await;
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.rows.lock().unwrap().clear();
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
    let mut second_config = fixture.config.clone();
    second_config.state_path = fixture.root.path().join("second-state.json");
    let mut first = Bridge::open(fixture.config.clone()).unwrap();
    let mut second = Bridge::open(second_config).unwrap();
    let key = agent().key();
    let group = "bb".repeat(16);
    let (a, b) = tokio::join!(
        first.attach_room(&key, &group, "one"),
        second.attach_room(&key, &group, "two")
    );
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "Exactly one transport state may reserve this native identity"
    );
}

#[tokio::test]
async fn room_dispatch_roster_drift_latches_and_consumes_input_across_restart() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    let input = event(&fixture, 180, HEX, "roster changes before dispatch", true);
    fixture.info.lock().unwrap()["room_members_after_read"] =
        json!([HEX, "33".repeat(32), "aa".repeat(32)]);
    bridge.inbound(&input).await.unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        bridge.state.mappings[&agent().key()]
            .room
            .as_ref()
            .unwrap()
            .seen(&format!("{:064x}", 180))
    );
    drop(bridge);
    fixture.info.lock().unwrap()["room_members"] =
        json!([HEX, "22".repeat(32), "33".repeat(32), "aa".repeat(32)]);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge
        .inbound(&event(&fixture, 181, HEX, "old roster is still held", true))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    classify(&mut bridge).await;
    bridge.inbound(&input).await.unwrap();
    assert!(
        fixture.posts.lock().unwrap().is_empty(),
        "Owner reconciliation cannot replay a consumed input"
    );
    bridge
        .inbound(&event(&fixture, 182, HEX, "new reconciled input", true))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn room_copied_state_cannot_admit_or_forward_under_another_configuration() {
    let (fixture, mut first) = room().await;
    classify(&mut first).await;
    grant(&mut first, HEX, "human").await;
    let mut second_config = fixture.config.clone();
    second_config.state_path = fixture.root.path().join("copied-state.json");
    let mut second = Bridge::open(second_config.clone()).unwrap();
    second.state = first.state.clone();
    second.save().unwrap();
    second.reconcile(fixture.snapshot()).await.unwrap();
    let owned = event(&fixture, 190, HEX, "owned invocation", true);
    let copied = event(&fixture, 191, HEX, "copied ownership", true);
    let (a, b) = tokio::join!(first.inbound(&owned), second.inbound(&copied));
    a.unwrap();
    let _ = b;
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    assert!(
        second.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    drop(second);
    let mut second = Bridge::open(second_config).unwrap();
    second.reconcile(fixture.snapshot()).await.unwrap();
    second.flush().await.unwrap();
    assert!(
        second.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "send_final")
    );
}

#[tokio::test]
async fn room_independent_transports_keep_directional_grants_and_native_ownership() {
    let (first_fixture, mut first) = room().await;
    let members = json!([
        HEX,
        "22".repeat(32),
        "33".repeat(32),
        "aa".repeat(32),
        "bb".repeat(32)
    ]);
    let roles = BTreeMap::from([
        (HEX.to_owned(), "human"),
        ("22".repeat(32), "human"),
        ("33".repeat(32), "agent"),
        ("aa".repeat(32), "agent"),
        ("bb".repeat(32), "agent"),
    ]);
    first_fixture.info.lock().unwrap()["room_members"] = members.clone();
    control(&mut first, "room.roster", json!({"roles":roles}))
        .await
        .unwrap();
    grant(&mut first, HEX, "human").await;
    let mut other = Fixture::new().await;
    other.config.native_claims_fixture_root =
        first_fixture.config.native_claims_fixture_root.clone();
    other.config.account_id_hex = "bb".repeat(32);
    other.info.lock().unwrap()["account_id_hex"] = json!(other.config.account_id_hex);
    other.info.lock().unwrap()["room_members"] = members;
    other
        .histories
        .lock()
        .unwrap()
        .insert("ses_other".into(), vec![]);
    other.agents.lock().unwrap()[0]
        .agent_session
        .as_mut()
        .unwrap()
        .value = "ses_other".into();
    let key = other.agents.lock().unwrap()[0].key();
    let mut second = Bridge::open(other.config.clone()).unwrap();
    second
        .attach_room(&key, &"bb".repeat(16), "reviewer")
        .await
        .unwrap();
    second.reconcile(other.snapshot()).await.unwrap();
    porter::handle(
        &mut second,
        json!({"version":1,"bridge_account":other.config.account_id_hex,"session_key":key,
        "method":"room.roster","roles":roles}),
    )
    .await
    .unwrap();
    first
        .inbound(&event(&first_fixture, 200, HEX, "first native only", true))
        .await
        .unwrap();
    second
        .inbound(&event(&other, 201, HEX, "not granted on second", true))
        .await
        .unwrap();
    assert!(
        other.posts.lock().unwrap().is_empty(),
        "First target grants cannot authorize the second target"
    );
    porter::handle(
        &mut second,
        json!({"version":1,"bridge_account":other.config.account_id_hex,"session_key":key,
        "method":"room.grant","account":HEX,"role":"human","allowed":true}),
    )
    .await
    .unwrap();
    second
        .inbound(&event(&other, 202, HEX, "second native only", true))
        .await
        .unwrap();
    assert_eq!(first_fixture.posts.lock().unwrap()[0].0, "ses_1");
    assert_eq!(other.posts.lock().unwrap()[0].0, "ses_other");
    assert_eq!(first.state.mappings.len(), 1);
    assert_eq!(second.state.mappings.len(), 1);
}

#[tokio::test]
async fn room_offline_attachment_has_no_discovery_invitation_or_send_side_effect() {
    let fixture = Fixture::new().await;
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.rows.lock().unwrap().clear();
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
    assert!(!fixture.config.control_chat);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    // This is the exact offline CLI seam, deliberately without run/reconcile.
    bridge
        .attach_room(&agent().key(), &"bb".repeat(16), "builder")
        .await
        .unwrap();
    assert_eq!(bridge.state.mappings.len(), 1);
    assert!(bridge.state.outbox.is_empty());
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(!fixture.calls.lock().unwrap().iter().any(|call| matches!(
        call["type"].as_str(),
        Some("group_create" | "group_create_with_admins" | "send_final" | "human_account_send")
    )));
}

#[tokio::test]
async fn room_claim_blocks_a_cached_private_terminal_and_its_queued_output() {
    let fixture = Fixture::new().await;
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.rows.lock().unwrap().clear();
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
    let mut private_config = fixture.config.clone();
    private_config.state_path = fixture.root.path().join("private-state.json");
    let mut private = Bridge::open(private_config).unwrap();
    private
        .state
        .mappings
        .insert(agent().key(), activity_mapping(agent(), &"dd".repeat(16)));
    private.reconcile(vec![agent()]).await.unwrap();
    let mut room = Bridge::open(fixture.config.clone()).unwrap();
    room.attach_room(&agent().key(), &"bb".repeat(16), "builder")
        .await
        .unwrap();
    // No new private snapshot: exercise the cached terminal route with V2
    // delivery removed, not only the normal discovery refusal.
    private.config.opencode_service_file = None;
    let mut input = message(HEX, "stale-private", "private native input");
    input["group_id_hex"] = json!("dd".repeat(16));
    private.inbound(&input).await.unwrap();
    assert!(
        private.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    private
        .state
        .enqueue_manual(&"dd".repeat(16), "must not be disclosed", "stale-output");
    private.flush().await.unwrap();
    assert!(private.state.outbox.is_empty());
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "agent.prompt" || call["type"] == "send_final")
    );
}

#[tokio::test]
async fn room_cannot_take_a_real_private_attachment_before_its_first_prompt() {
    let fixture = Fixture::new().await;
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.rows.lock().unwrap().clear();
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
    let mut private = Bridge::open(fixture.config.clone()).unwrap();
    private
        .attach_chat(&agent().key(), &"bb".repeat(16), &app_npub(0x22))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());

    let mut room_config = fixture.config.clone();
    room_config.state_path = fixture.root.path().join("other-room-state.json");
    let mut room = Bridge::open(room_config).unwrap();
    assert!(
        room.attach_room(&agent().key(), &"bb".repeat(16), "builder")
            .await
            .is_err(),
        "A successful real private attachment owns even an empty conversation"
    );
    assert!(room.state.mappings.is_empty());
    private.reconcile(vec![agent()]).await.unwrap();
    assert!(
        private.state.mappings[&agent().key()]
            .room_quarantined
            .is_none()
    );
    let mut input = message(HEX, "first-private-prompt", "private remains usable");
    input["message"]["recorded_at"] = json!(now().saturating_add(1));
    private.inbound(&input).await.unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn room_cannot_take_a_newly_discovered_private_binding_before_its_first_prompt() {
    let fixture = Fixture::new().await;
    fixture
        .histories
        .lock()
        .unwrap()
        .get_mut("ses_1")
        .unwrap()
        .clear();
    fixture.rows.lock().unwrap().clear();
    fixture.info.lock().unwrap()["room_members"] = json!([HEX, "aa".repeat(32)]);
    let mut private = Bridge::open(fixture.config.clone()).unwrap();
    private.reconcile(vec![agent()]).await.unwrap();
    assert_eq!(
        private.state.mappings[&agent().key()].group_id_hex,
        Some("ee".repeat(16))
    );
    assert!(fixture.posts.lock().unwrap().is_empty());
    let mut room_config = fixture.config.clone();
    room_config.state_path = fixture.root.path().join("other-room-state.json");
    let mut room = Bridge::open(room_config).unwrap();
    assert!(
        room.attach_room(&agent().key(), &"bb".repeat(16), "builder")
            .await
            .is_err(),
        "Creating a private chat establishes ownership without a first prompt"
    );
}
