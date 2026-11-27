//! Phone controls resolve real MDK fixture records; incoming envelopes lie.
use super::*;

fn revision(bridge: &Bridge) -> String {
    bridge
        .state
        .outbox
        .iter()
        .rev()
        .find_map(|item| {
            item.text
                .lines()
                .find_map(|line| line.strip_prefix("Revision: ").map(str::to_owned))
        })
        .expect("Owner control reply must carry a target-bound revision")
}

fn policy(bridge: &Bridge) -> Value {
    serde_json::to_value(bridge.state.mappings[&agent().key()].room.as_ref().unwrap()).unwrap()
}

#[tokio::test]
async fn owner_controls_allow_revoke_while_stopped_revoked_and_native_unavailable() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    control(&mut bridge, "room.stop", json!({})).await.unwrap();
    bridge.config.opencode_service_file = None;
    fixture.failed_reads.lock().unwrap().push("ses_1".into());
    let before = fixture.calls.lock().unwrap().len();
    bridge
        .inbound(&event(&fixture, 1000, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let token = revision(&bridge);
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .any(|item| item.text.contains(&"22".repeat(32)) && item.text.contains("denied"))
    );
    bridge
        .inbound(&event(
            &fixture,
            1001,
            HEX,
            &format!("/agents @builder allow {} {token}", "22".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(policy(&bridge)["humans_allowed"], json!(["22".repeat(32)]));
    let token = revision(&bridge);
    bridge
        .inbound(&event(
            &fixture,
            1002,
            HEX,
            &format!("/agents @builder revoke {} {token}", "22".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(policy(&bridge)["humans_allowed"], json!([]));
    assert_eq!(policy(&bridge)["stopped"], true);
    assert_eq!(policy(&bridge)["agent_remaining"], 0);
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        fixture.calls.lock().unwrap()[before..]
            .iter()
            .all(|call| call["type"] == "room_context")
    );
    let saved = BridgeState::load(&bridge.config.state_path).unwrap();
    assert_eq!(
        saved.outbox, bridge.state.outbox,
        "Grant, dedup and acknowledgement intent must be saved together"
    );
}

#[tokio::test]
async fn owner_controls_deny_admin_agent_bridge_spoof_and_wrong_or_missing_target() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    // Membership/admin claims in the envelope cannot replace the signed sender.
    for (sequence, sender, text, addressed) in [
        (1010, "22".repeat(32), "/agents @builder", false),
        (1011, "33".repeat(32), "/agents @builder", true),
        (1012, "aa".repeat(32), "/agents @builder", true),
        (1013, HEX.to_owned(), "/agents @reviewer", true),
        (1014, HEX.to_owned(), "/agents", false),
    ] {
        let mut incoming = event(&fixture, sequence, &sender, text, addressed);
        incoming["message"]["sender"] =
            json!({"account_id_hex":HEX,"is_admin":true,"role":"owner"});
        bridge.inbound(&incoming).await.unwrap();
    }
    assert!(bridge.state.outbox.is_empty());
    assert!(fixture.posts.lock().unwrap().is_empty());
    bridge
        .inbound(&event(&fixture, 1015, HEX, "/agents", true))
        .await
        .unwrap();
    assert!(!revision(&bridge).is_empty());
}

#[tokio::test]
async fn owner_controls_replay_restart_and_stale_revision_cannot_restore_a_grant() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    bridge
        .inbound(&event(&fixture, 1020, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let old = revision(&bridge);
    let allow = event(
        &fixture,
        1021,
        HEX,
        &format!("/agents @builder allow {} {old}", "22".repeat(32)),
        false,
    );
    bridge.inbound(&allow).await.unwrap();
    let next = revision(&bridge);
    bridge
        .inbound(&event(
            &fixture,
            1022,
            HEX,
            &format!("/agents @builder revoke {} {next}", "22".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    let count = bridge.state.outbox.len();
    drop(bridge);
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.inbound(&allow).await.unwrap();
    assert_eq!(
        bridge.state.outbox.len(),
        count,
        "Duplicate controls do not acknowledge or apply again"
    );
    bridge
        .inbound(&event(
            &fixture,
            1023,
            HEX,
            &format!("/agents @builder allow {} {old}", "22".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(policy(&bridge)["humans_allowed"], json!([]));
    assert!(bridge.state.outbox.last().unwrap().text.contains("stale"));
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn owner_controls_never_renew_a_spent_agent_budget_or_invoke_private_commands() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, HEX, "human").await;
    grant(&mut bridge, &"33".repeat(32), "agent").await;
    control(&mut bridge, "room.agent_budget", json!({"enabled":true}))
        .await
        .unwrap();
    bridge
        .inbound(&event(&fixture, 1030, HEX, "new cycle", true))
        .await
        .unwrap();
    for sequence in 1031..1035 {
        bridge
            .inbound(&event(
                &fixture,
                sequence,
                &"33".repeat(32),
                "agent work",
                true,
            ))
            .await
            .unwrap();
    }
    assert_eq!(policy(&bridge)["agent_remaining"], 0);
    bridge
        .inbound(&event(&fixture, 1035, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let token = revision(&bridge);
    bridge
        .inbound(&event(
            &fixture,
            1036,
            HEX,
            &format!("/agents @builder allow {} {token}", app_npub(0x22)),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(policy(&bridge)["agent_remaining"], 0);
    bridge
        .inbound(&event(
            &fixture,
            1037,
            &"33".repeat(32),
            "no renewed budget",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 5);
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
async fn owner_controls_hold_changed_rosters_and_reject_host_stale_and_old_messages() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    bridge
        .inbound(&event(&fixture, 1040, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let token = revision(&bridge);
    // A host grant/revoke round trip must invalidate old phone choices (ABA).
    grant(&mut bridge, &"22".repeat(32), "human").await;
    control(
        &mut bridge,
        "room.grant",
        json!({"account":"22".repeat(32),"role":"human","allowed":false}),
    )
    .await
    .unwrap();
    bridge
        .inbound(&event(
            &fixture,
            1041,
            HEX,
            &format!("/agents @builder allow {} {token}", "22".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    assert!(
        policy(&bridge)["humans_allowed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(bridge.state.outbox.last().unwrap().text.contains("stale"));
    bridge
        .inbound(&event(&fixture, 1042, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let token = revision(&bridge);
    let old = event(
        &fixture,
        1043,
        HEX,
        &format!("/agents @builder allow {} {token}", "22".repeat(32)),
        false,
    );
    fixture.info.lock().unwrap()["room_messages"][format!("{:064x}", 1043)]["recorded_at"] =
        json!(0);
    let count = bridge.state.outbox.len();
    bridge.inbound(&old).await.unwrap();
    assert_eq!(bridge.state.outbox.len(), count);
    fixture.info.lock().unwrap()["room_members_after_read"] = json!([
        HEX,
        "22".repeat(32),
        "33".repeat(32),
        "aa".repeat(32),
        "55".repeat(32)
    ]);
    bridge
        .inbound(&event(
            &fixture,
            1044,
            HEX,
            &format!("/agents @builder allow {} {token}", "22".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    assert!(
        policy(&bridge)["humans_allowed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("room.roster")
    );
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn owner_controls_show_reconciliation_instead_of_inventing_member_roles() {
    let (fixture, mut bridge) = room().await;
    bridge
        .inbound(&event(&fixture, 1050, HEX, "/agents @builder", false))
        .await
        .unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("room.roster")
    );
    assert!(
        policy(&bridge)["humans_allowed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        policy(&bridge)["agents_allowed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(fixture.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn owner_controls_keep_target_owners_and_revision_namespaces_independent() {
    let (fixture, mut first) = room().await;
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
    fixture.info.lock().unwrap()["room_members"] = members.clone();
    control(&mut first, "room.roster", json!({"roles":roles}))
        .await
        .unwrap();
    first
        .inbound(&event(&fixture, 1060, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let a_token = revision(&first);
    let mut other = Fixture::new().await;
    other.config.native_claims_fixture_root = fixture.config.native_claims_fixture_root.clone();
    other.config.account_id_hex = "bb".repeat(32);
    other.config.phone_npub = app_npub(0x22);
    other.info.lock().unwrap()["account_id_hex"] = json!(other.config.account_id_hex);
    other.info.lock().unwrap()["room_members"] = members;
    other
        .histories
        .lock()
        .unwrap()
        .insert("ses_owner_b".into(), vec![]);
    other.agents.lock().unwrap()[0]
        .agent_session
        .as_mut()
        .unwrap()
        .value = "ses_owner_b".into();
    let key = other.agents.lock().unwrap()[0].key();
    let mut second = Bridge::open(other.config.clone()).unwrap();
    second
        .attach_room(&key, &"bb".repeat(16), "reviewer")
        .await
        .unwrap();
    porter::handle(
        &mut second,
        json!({"version":1,"bridge_account":other.config.account_id_hex,
        "method":"room.roster","session_key":key,"roles":roles}),
    )
    .await
    .unwrap();
    second
        .inbound(&event(&other, 1061, HEX, "/agents @reviewer", true))
        .await
        .unwrap();
    assert!(
        second.state.outbox.is_empty(),
        "Owner of A is not owner of B"
    );
    second
        .inbound(&event(
            &other,
            1062,
            &"22".repeat(32),
            "/agents @reviewer",
            false,
        ))
        .await
        .unwrap();
    let b_token = revision(&second);
    assert_ne!(
        a_token, b_token,
        "Menu choices are bound to the transport and owner, not just a counter"
    );
    second
        .inbound(&event(
            &other,
            1063,
            &"22".repeat(32),
            &format!("/agents @reviewer allow {HEX} {a_token}"),
            false,
        ))
        .await
        .unwrap();
    assert!(second.state.outbox.last().unwrap().text.contains("stale"));
    second
        .inbound(&event(
            &other,
            1064,
            &"22".repeat(32),
            &format!("/agents @reviewer allow {HEX} {b_token}"),
            false,
        ))
        .await
        .unwrap();
    let current = serde_json::to_value(second.state.mappings[&key].room.as_ref().unwrap()).unwrap();
    assert_eq!(current["humans_allowed"], json!([HEX]));
    let count = second.state.outbox.len();
    second
        .inbound(&event(&other, 1065, HEX, "/agents @reviewer", false))
        .await
        .unwrap();
    assert_eq!(
        second.state.outbox.len(),
        count,
        "An invocation grant never grants access administration"
    );
    assert!(fixture.posts.lock().unwrap().is_empty() && other.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn owner_controls_preserve_authenticated_model_addressing_and_agent_enablement() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    bridge
        .inbound(&event(&fixture, 1070, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let token = revision(&bridge);
    bridge
        .inbound(&event(
            &fixture,
            1071,
            HEX,
            &format!("/agents @builder allow {HEX} {token}"),
            false,
        ))
        .await
        .unwrap();
    let token = revision(&bridge);
    bridge
        .inbound(&event(
            &fixture,
            1072,
            HEX,
            &format!("/agents @builder allow {} {token}", "33".repeat(32)),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(policy(&bridge)["agents_allowed"], json!(["33".repeat(32)]));
    assert_eq!(policy(&bridge)["agent_enabled"], false);
    bridge
        .inbound(&event(
            &fixture,
            1073,
            HEX,
            "@builder text is not an authenticated mention",
            false,
        ))
        .await
        .unwrap();
    bridge
        .inbound(&event(
            &fixture,
            1074,
            &"33".repeat(32),
            "not enabled",
            true,
        ))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    bridge
        .inbound(&event(&fixture, 1075, HEX, "proper human invocation", true))
        .await
        .unwrap();
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn owner_controls_public_tokens_do_not_authorize_nonowners_or_media() {
    let (fixture, mut bridge) = room().await;
    classify(&mut bridge).await;
    grant(&mut bridge, &"33".repeat(32), "agent").await;
    bridge
        .inbound(&event(&fixture, 1080, HEX, "/agents @builder", false))
        .await
        .unwrap();
    let token = revision(&bridge);
    let command = format!("/agents @builder allow {} {token}", "22".repeat(32));
    let count = bridge.state.outbox.len();
    for (sequence, sender) in [
        (1081, "22".repeat(32)),
        (1082, "33".repeat(32)),
        (1083, "aa".repeat(32)),
    ] {
        let mut incoming = event(&fixture, sequence, &sender, &command, true);
        incoming["message"]["sender"] = json!({"account_id_hex":HEX,"is_admin":true});
        bridge.inbound(&incoming).await.unwrap();
    }
    let media = event(&fixture, 1084, HEX, &command, true);
    fixture.info.lock().unwrap()["room_messages"][format!("{:064x}", 1084)]["unsupported_media"] =
        json!(true);
    bridge.inbound(&media).await.unwrap();
    let other_target = event(&fixture, 1085, HEX, &command, false);
    fixture.info.lock().unwrap()["room_messages"][format!("{:064x}", 1085)]["addressed_accounts"] =
        json!(["bb".repeat(32)]);
    bridge.inbound(&other_target).await.unwrap();
    assert_eq!(bridge.state.outbox.len(), count);
    assert_eq!(policy(&bridge)["humans_allowed"], json!([]));
    assert_eq!(policy(&bridge)["agent_remaining"], 0);
    assert!(fixture.posts.lock().unwrap().is_empty());
}
