use bech32::ToBase32;
use herdr_white_noise_bridge::{BridgeConfig, bridge::Bridge, state::Mapping, update};
use serde_json::{Value, json};

fn setup(root: &std::path::Path) -> Bridge {
    let config: BridgeConfig = serde_json::from_value(json!({
        "phone_npub": bech32::encode("npub", [0x11u8;32].to_base32(), bech32::Variant::Bech32).unwrap(),
        "account_id_hex":"aa".repeat(32), "herdr_socket":root.join("h.sock"),
        "wn_agent_socket":root.join("w.sock"), "state_path":root.join("state.json")
    })).unwrap();
    let mut bridge = Bridge::open(config).unwrap();
    for (key, group) in [("one", "bb"), ("two", "cc")] {
        bridge.state.mappings.insert(
            key.into(),
            Mapping {
                group_id_hex: Some(group.repeat(16)),
                active: true,
                pending_prompt: true,
                seen_messages: vec!["existing-receipt".into()],
                ..Mapping::default()
            },
        );
    }
    bridge.update_config_path = Some(root.join("config.toml"));
    bridge.save().unwrap();
    bridge
}

fn release() -> update::Release {
    let version = format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH").parse::<u64>().unwrap() + 1
    );
    let filename = format!("codr-v{version}-linux-x86_64.tar.gz");
    serde_json::from_value(json!({"version":version, "event_id":"ab".repeat(32), "commit":"cd".repeat(20),
        "asset":{"filename":filename,"url":"https://blossom.example/archive","sha256":"ef".repeat(32),"size":"100"},
        "checksum":{"filename":format!("{filename}.sha256"),"url":"https://blossom.example/checksum","sha256":"12".repeat(32),"size":"100"}})).unwrap()
}

#[test]
fn default_probe_keeps_the_shipped_090_contract_and_full_probe_is_explicit() {
    let binary = env!("CARGO_BIN_EXE_herdr-white-noise-bridge");
    let output = std::process::Command::new(binary)
        .arg("update-info")
        .output()
        .unwrap();
    assert!(output.status.success());
    let legacy: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(legacy.get("runtimes").is_none());
    assert!(legacy.get("storage_cohort").is_none());
    let output = std::process::Command::new(binary)
        .args(["update-info", "--full"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let full: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(full, update::info().unwrap());
    assert_eq!(legacy["connector"], full["connector"]);
    assert_eq!(legacy["protocol"], full["protocol"]);
    assert_eq!(legacy["state_version"], full["state_version"]);
}

#[test]
fn shipped_090_installer_accepts_the_real_binary_without_changing_the_connector() {
    let root = tempfile::tempdir().unwrap();
    let bridge = setup(root.path());
    std::fs::write(root.path().join("config.toml"), format!(
        "phone_npub = {:?}\naccount_id_hex = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\nstate_path = {:?}\n",
        bridge.config.phone_npub, bridge.config.account_id_hex, bridge.config.herdr_socket,
        bridge.config.wn_agent_socket, bridge.config.state_path)).unwrap();
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/check_090_upgrade.py"
        ))
        .arg(root.path())
        .arg(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn external_mode_offers_local_handover_without_fabricating_a_release() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    bridge
        .inbound(&event("handover-check", "/update"))
        .await
        .unwrap();
    let offer = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .expect("local handover offer");
    let value = serde_json::to_value(offer).unwrap();
    assert!(value["release"].is_null());
    assert_eq!(value["handover"], true);
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("cold backup")
    );
    let code = offer.code.clone();
    let job = update::prepare(
        &mut bridge,
        "one",
        &"bb".repeat(16),
        "handover-confirm",
        &code,
    )
    .unwrap();
    let request: Value =
        serde_json::from_slice(&std::fs::read(job.join("request.json")).unwrap()).unwrap();
    assert_eq!(request["action"], "handover");
    assert!(request["release"].is_null());
    assert!(job.join("handover.py").is_file());
    assert!(job.join("codr.service").is_file());
}

fn event(id: &str, text: &str) -> Value {
    json!({"marmot_agent_control":"marmot.agent-control.v2","type":"inbound_message",
        "account_id_hex":"aa".repeat(32),"group_id_hex":"bb".repeat(16),
        "message":{"message_id_hex":id,"sender":{"account_id_hex":"11".repeat(32)},"text":text}})
}

#[test]
fn offer_is_persistent_chat_bound_and_wrong_or_expired_confirmation_has_no_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let offer = bridge.state.mappings["one"].update_offer.clone().unwrap();
    let config = bridge.config.clone();
    let mut bridge = Bridge::open(config).unwrap();
    bridge.update_config_path = Some(root.path().join("config.toml"));
    assert_eq!(
        bridge.state.mappings["one"]
            .update_offer
            .as_ref()
            .unwrap()
            .code,
        offer.code
    );
    assert!(
        update::prepare(
            &mut bridge,
            "two",
            &"cc".repeat(16),
            "foreign-chat",
            &offer.code
        )
        .is_err()
    );
    assert!(update::prepare(&mut bridge, "one", &"bb".repeat(16), "wrong-code", "wrong").is_err());
    bridge
        .state
        .mappings
        .get_mut("one")
        .unwrap()
        .update_offer
        .as_mut()
        .unwrap()
        .expires_at = 0;
    assert!(update::prepare(&mut bridge, "one", &"bb".repeat(16), "expired", &offer.code).is_err());
    assert!(bridge.state.update_job.is_none());
    assert!(bridge.state.mappings["one"].pending_prompt);
}

#[tokio::test]
async fn confirmation_is_durable_before_launch_and_completion_goes_only_to_original_chat() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let code = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .unwrap()
        .code
        .clone();
    let directory =
        update::prepare(&mut bridge, "one", &"bb".repeat(16), "confirm", &code).unwrap();
    assert!(bridge.state.mappings["one"].update_offer.is_none());
    assert!(
        bridge.state.mappings["one"]
            .seen_messages
            .iter()
            .any(|id| id == "confirm")
    );
    let job = bridge.state.update_job.clone().unwrap();
    let mut bridge = Bridge::open(bridge.config.clone()).unwrap();
    assert!(bridge.state.update_job.is_some());
    assert!(update::prepare(&mut bridge, "one", &"bb".repeat(16), "repeat", &code).is_err());
    update::ready(&bridge).unwrap();
    let ready: Value =
        serde_json::from_slice(&std::fs::read(directory.join("ready.json")).unwrap()).unwrap();
    assert_eq!(ready["job_id"], job.id);
    assert_eq!(ready["version"], env!("CARGO_PKG_VERSION"));
    std::fs::write(directory.join("result.json"), serde_json::to_vec(&json!({"version":job.version,"job_id":job.id,"status":"installed","message":format!("Updated codr to {}.",job.version)})).unwrap()).unwrap();
    update::poll(&mut bridge).await.unwrap();
    assert!(bridge.state.update_job.is_none());
    assert_eq!(bridge.state.outbox.last().unwrap().group, "bb".repeat(16));
    assert!(bridge.state.mappings["one"].pending_prompt);
    assert!(
        bridge.state.mappings["one"]
            .seen_messages
            .iter()
            .any(|id| id == "existing-receipt")
    );
    let count = bridge.state.outbox.len();
    update::poll(&mut bridge).await.unwrap();
    assert_eq!(bridge.state.outbox.len(), count);
}

#[tokio::test]
async fn update_controls_never_reach_agent_and_reject_other_senders_or_attachments() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    let mut foreign = event("foreign", "/update cancel");
    foreign["message"]["sender"]["account_id_hex"] = json!("22".repeat(32));
    bridge.inbound(&foreign).await.unwrap();
    assert!(bridge.state.outbox.is_empty());
    let mut attachment = event("attachment", "/update confirm code");
    attachment["message"]["attachments"] = json!([{"media_type":"image/png"}]);
    bridge.inbound(&attachment).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("standalone")
    );
    bridge
        .inbound(&event("cancel", "/update cancel"))
        .await
        .unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("cancelled")
    );
    let count = bridge.state.outbox.len();
    bridge
        .inbound(&event("cancel", "/update cancel"))
        .await
        .unwrap();
    assert_eq!(bridge.state.outbox.len(), count);
    assert!(bridge.state.update_job.is_none());
    assert!(bridge.state.mappings["one"].pending_prompt);
}

#[tokio::test]
async fn unsupported_managed_install_does_not_leave_a_false_in_progress_job() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let code = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .unwrap()
        .code
        .clone();
    // The test executable is not the standard installed worker. Preflight must
    // refuse it before making any service-manager or network calls.
    bridge
        .inbound(&event(
            "confirm-unsupported",
            &format!("/update confirm {code}"),
        ))
        .await
        .unwrap();
    assert!(bridge.state.update_job.is_none());
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("nothing was installed")
    );
    assert!(bridge.state.mappings["one"].pending_prompt);
}

#[tokio::test]
async fn a_result_for_another_job_cannot_clear_the_pending_installation() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let code = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .unwrap()
        .code
        .clone();
    let directory =
        update::prepare(&mut bridge, "one", &"bb".repeat(16), "confirm", &code).unwrap();
    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec(&json!({"job_id":"other","version":release().version,"status":"installed","message":"success"})).unwrap(),
    )
    .unwrap();
    assert!(update::poll(&mut bridge).await.is_err());
    let before = bridge.state.update_job.clone();
    bridge
        .inbound(&event("cancel-running", "/update cancel"))
        .await
        .unwrap();
    assert_eq!(bridge.state.update_job, before);
    assert!(bridge.state.outbox.last().unwrap().text.contains("pending"));
}

#[test]
fn failed_confirmation_save_preserves_offer_and_does_not_consume_input() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let code = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .unwrap()
        .code
        .clone();
    let before = serde_json::to_value(&bridge.state).unwrap();
    std::fs::remove_file(&bridge.config.state_path).unwrap();
    std::fs::create_dir(&bridge.config.state_path).unwrap();
    assert!(update::prepare(&mut bridge, "one", &"bb".repeat(16), "confirm", &code).is_err());
    assert_eq!(serde_json::to_value(&bridge.state).unwrap(), before);
}

#[test]
fn a_stale_offer_cannot_reinstall_or_downgrade_the_current_version() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    let mut offer = release();
    offer.version = env!("CARGO_PKG_VERSION").into();
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", offer).unwrap();
    let code = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .unwrap()
        .code
        .clone();
    assert!(update::prepare(&mut bridge, "one", &"bb".repeat(16), "stale", &code).is_err());
    assert!(bridge.state.update_job.is_none());
}

#[tokio::test]
async fn code_free_confirmation_uses_a_fresh_offer_and_never_asks_for_a_code() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let text = &bridge.state.outbox.last().unwrap().text;
    assert!(text.contains("\n/update confirm\n"));
    let offer = bridge.state.mappings["one"].update_offer.clone().unwrap();
    assert!(!text.contains(&offer.code));
    // Age the offer, not the clock, so the transport event is strictly newer.
    bridge
        .state
        .mappings
        .get_mut("one")
        .unwrap()
        .update_offer
        .as_mut()
        .unwrap()
        .expires_at -= 2;
    let mut confirm = event("plain-confirm", "/update confirm");
    confirm["message"]["recorded_at"] = json!(herdr_white_noise_bridge::bridge::now());
    bridge.inbound(&confirm).await.unwrap();
    // The isolated test binary cannot pass managed-install preflight. Reaching
    // that refusal proves the code-free route prepared and launched its helper.
    assert!(bridge.state.mappings["one"].update_offer.is_none());
    assert!(bridge.state.update_job.is_none());
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("nothing was installed")
    );
    let count = bridge.state.outbox.len();
    bridge.inbound(&confirm).await.unwrap();
    assert_eq!(bridge.state.outbox.len(), count);
}

#[tokio::test]
async fn code_free_confirmation_rejects_stale_or_unverifiable_transport_times() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    let before = bridge.state.mappings["one"].update_offer.clone();
    let mut foreign = event("foreign-confirm", "/update confirm");
    foreign["message"]["sender"]["account_id_hex"] = json!("22".repeat(32));
    foreign["message"]["recorded_at"] = json!(herdr_white_noise_bridge::bridge::now());
    bridge.inbound(&foreign).await.unwrap();
    assert_eq!(bridge.state.mappings["one"].update_offer, before);
    for (id, at) in [
        ("missing-time", None),
        ("stale-time", Some(1)),
        ("future-time", Some(u64::MAX)),
    ] {
        let mut confirm = event(id, "/update confirm");
        if let Some(at) = at {
            confirm["message"]["recorded_at"] = json!(at);
        }
        bridge.inbound(&confirm).await.unwrap();
        assert_eq!(bridge.state.mappings["one"].update_offer, before);
        assert!(bridge.state.update_job.is_none());
        assert!(
            !bridge
                .state
                .outbox
                .last()
                .unwrap()
                .text
                .contains("Use /update,")
        );
    }
}

#[tokio::test]
async fn starting_a_new_check_clears_the_old_offer_before_the_background_check() {
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    bridge.update_messaging_home = Some(root.path().join("mdk"));
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    bridge.inbound(&event("refresh", "/update")).await.unwrap();
    assert!(bridge.state.mappings["one"].update_offer.is_none());
    // Current-thread Tokio has not polled the spawned check; Drop cancels it.
    drop(bridge);
}

#[tokio::test]
async fn combined_update_records_home_and_waits_for_messaging_readiness() {
    use tokio::{io::AsyncWriteExt, net::UnixListener};
    let root = tempfile::tempdir().unwrap();
    let mut bridge = setup(root.path());
    let home = root.path().join("messaging");
    std::fs::create_dir(&home).unwrap();
    bridge.update_messaging_home = Some(home.clone());
    update::store_offer(&mut bridge, "one", &"bb".repeat(16), "check", release()).unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("messaging executable will update together")
    );
    let code = bridge.state.mappings["one"]
        .update_offer
        .as_ref()
        .unwrap()
        .code
        .clone();
    let directory =
        update::prepare(&mut bridge, "one", &"bb".repeat(16), "confirm", &code).unwrap();
    let request: Value =
        serde_json::from_slice(&std::fs::read(directory.join("request.json")).unwrap()).unwrap();
    assert_eq!(request["service"], "codr");
    assert_eq!(request["messaging_home"], home.to_str().unwrap());
    assert_eq!(request["info"]["storage_cohort"], "mdk-0.14");
    let socket = bridge.config.wn_agent_socket.clone();
    let account = bridge.config.account_id_hex.clone();
    let worker = tokio::spawn(async move { bridge.run(false).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let early = directory.join("ready.json").exists();
    let listener = UnixListener::bind(&socket).unwrap();
    let messaging = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let request = herdr_white_noise_bridge::wire::read_line(&mut stream)
                .await
                .unwrap();
            let response = json!({"id":request["id"], "marmot_agent_control":"marmot.agent-control.v2", "type":"account_list", "accounts":[{"account_id_hex":account,"local_signing":true}]});
            let _ = stream
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await;
        }
    });
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !directory.join("ready.json").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    worker.abort();
    messaging.abort();
    assert!(
        !early,
        "combined readiness must not be published before messaging works"
    );
    ready.unwrap();
}
