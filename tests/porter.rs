use bech32::ToBase32;
use herdr_white_noise_bridge::{
    BridgeConfig, bridge::Bridge, herdr::Agent, porter, state::Mapping, wire::read_line,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
};

// These fixtures share Porter's process-wide four-reader limit. Serialize only
// successful spawned-read tests so unrelated parallel tests cannot spend their
// capacity. Each still verifies that slow I/O leaves the bridge loop immediately.
static SPAWNED_READ_FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn config(dir: &Path) -> BridgeConfig {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    BridgeConfig {
        phone_npub: bech32::encode("npub", [0x11u8; 32].to_base32(), bech32::Variant::Bech32)
            .unwrap(),
        herdr_socket: dir.join("herdr.sock"),
        wn_agent_socket: dir.join("wn.sock"),
        account_id_hex: "aa".repeat(32),
        state_path: dir.join("state.json"),
        native_claims_fixture_root: Some(dir.join("native-claims")),
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
    serde_json::from_value(json!({"pane_id":"w1:p1","terminal_id":"term1","tab_id":"w1:t1",
        "agent":"opencode","agent_status":"working","state_change_seq":2,"cwd":"/repo",
        "agent_session":{"agent":"opencode","source":"herdr:opencode","kind":"id","value":"ses_1"}})).unwrap()
}

async fn host(dir: &Path) -> tokio::task::JoinHandle<()> {
    host_ready(dir).await.0
}

async fn host_ready(
    dir: &Path,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let listener = UnixListener::bind(dir.join("herdr.sock")).unwrap();
    let (ready, receiver) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let request = read_line(&mut stream).await.unwrap();
            if request["method"] == "session.snapshot"
                && let Some(ready) = ready.take()
            {
                let _ = ready.send(());
            }
            assert!(
                matches!(
                    request["method"].as_str(),
                    Some("session.snapshot" | "agent.get")
                ),
                "Porter must never bypass the queue"
            );
            let response = if request["method"] == "agent.get" {
                json!({"id":request["id"], "result":{"agent":agent()}})
            } else {
                json!({"id":request["id"],"result":{"snapshot":{"agents":[agent()],
                    "tabs":[{"tab_id":"w1:t1","label":"Fix parser"}]}}})
            };
            stream
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    (task, receiver)
}

fn enqueue() -> Value {
    json!({"version":1,"method":"enqueue","bridge_account":"aa".repeat(32),"request_id":"00112233-4455-4677-8899-aabbccddeeff",
        "session_key":agent().key(),"terminal_id":"term1","text":"Check parser"})
}

fn bridge(dir: &Path) -> Bridge {
    let mut bridge = Bridge::open(config(dir)).unwrap();
    bridge.state.mappings.insert(
        agent().key(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: true,
            ..Mapping::default()
        },
    );
    bridge.save().unwrap();
    bridge
}

#[tokio::test]
async fn snapshot_uses_live_herdr_and_plugin_account() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    let value = porter::handle(&mut bridge, json!({"version":1,"method":"snapshot"}))
        .await
        .unwrap();
    assert_eq!(value["bridge_account"], "aa".repeat(32));
    assert_eq!(value["sessions"][0]["agent"]["agent_status"], "working");
    assert_eq!(value["sessions"][0]["agent"]["tab_label"], "Fix parser");
    assert_eq!(value["sessions"][0]["session_key"], agent().key());
    assert_eq!(value["sessions"][0]["last_received_at"], 0);
    assert_eq!(value["sessions"][0]["reply_revision"], 0);
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .final_ids
        .push("reply-1".into());
    let next = porter::handle(&mut bridge, json!({"version":1,"method":"snapshot"}))
        .await
        .unwrap();
    assert_eq!(next["sessions"][0]["reply_revision"], 1);
    peer.abort();
}

#[tokio::test]
async fn enqueue_is_durable_idempotent_and_does_not_unmute() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    bridge.state.forwarding_enabled = false;
    let first = porter::handle(&mut bridge, enqueue()).await.unwrap();
    assert_eq!(first["status"], "queued");
    let second = porter::handle(&mut bridge, enqueue()).await.unwrap();
    assert_eq!(second["status"], "already_accepted");
    let mut receipt = enqueue();
    receipt["method"] = json!("receipt");
    assert_eq!(
        porter::handle(&mut bridge, receipt).await.unwrap()["accepted"],
        true
    );
    assert!(!bridge.state.forwarding_enabled);
    let saved = herdr_white_noise_bridge::BridgeState::load(&bridge.config.state_path).unwrap();
    assert_eq!(saved.mappings[&agent().key()].prompt_queue.len(), 1);
    assert_eq!(
        saved.mappings[&agent().key()].prompt_queue[0].text,
        "Check parser"
    );
    peer.abort();
}

#[tokio::test]
async fn stale_identity_invalid_text_and_full_queue_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    for change in [
        json!({"terminal_id":"replaced"}),
        json!({"text":"  "}),
        json!({"text":"a".repeat(30001)}),
        json!({"version":2}),
        json!({"request_id":"not-a-uuid"}),
        json!({"bridge_account":"00".repeat(32)}),
    ] {
        let mut request = enqueue();
        for (key, value) in change.as_object().unwrap() {
            request[key] = value.clone();
        }
        assert!(porter::handle(&mut bridge, request).await.is_err());
    }
    assert!(
        bridge.state.mappings[&agent().key()]
            .prompt_queue
            .is_empty()
    );
    for _ in 0..16 {
        let mut request = enqueue();
        request["request_id"] = json!(uuid::Uuid::new_v4().to_string());
        porter::handle(&mut bridge, request).await.unwrap();
    }
    let mut request = enqueue();
    request["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert!(porter::handle(&mut bridge, request).await.is_err());
    peer.abort();
}

#[tokio::test]
async fn listener_is_private_and_refuses_to_replace_regular_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let listener = porter::listen(&state).unwrap();
    let socket = porter::socket_path(&state);
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(listener);
    std::fs::remove_file(&socket).unwrap();
    std::fs::write(&socket, "do not overwrite").unwrap();
    assert!(porter::listen(&state).is_err());
    assert_eq!(std::fs::read_to_string(socket).unwrap(), "do not overwrite");
}

#[tokio::test]
async fn oversized_socket_request_is_rejected_before_state_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = bridge(dir.path());
    let before = bridge.state.clone();
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    let sender = tokio::spawn(async move {
        client
            .write_all(&vec![b'x'; herdr_white_noise_bridge::wire::MAX_FRAME + 1])
            .await
    });
    assert!(porter::serve(&mut bridge, server).await.is_err());
    assert_eq!(bridge.state, before);
    sender.abort();
}

#[tokio::test]
async fn slow_snapshot_does_not_block_the_bridge_event_loop() {
    let _fixture = SPAWNED_READ_FIXTURE.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(dir.path().join("herdr.sock")).unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        stream
            .get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"id":request["id"],"result":{"snapshot":{"agents":[agent()]}}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut bridge = bridge(dir.path());
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    client
        .write_all(b"{\"version\":1,\"id\":\"slow\",\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        porter::serve(&mut bridge, server),
    )
    .await
    .expect("slow snapshots must leave the bridge loop immediately")
    .unwrap();
    let response = read_line(&mut BufReader::new(client)).await.unwrap();
    assert_eq!(
        response["result"]["sessions"][0]["session_key"],
        agent().key()
    );
    peer.await.unwrap();
}

#[tokio::test]
async fn slow_history_does_not_block_the_bridge_event_loop() {
    let _fixture = SPAWNED_READ_FIXTURE.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    std::fs::write(
        &bridge.config.opencode_bin,
        "#!/bin/sh\nsleep 0.4\nprintf '%s' '{\"messages\":[]}'\n",
    )
    .unwrap();
    std::fs::set_permissions(
        &bridge.config.opencode_bin,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    let request = json!({"version":1,"bridge_account":"aa".repeat(32),"id":"wire1","request_id":"request1","method":"history",
        "session_key":agent().key(),"terminal_id":"term1"});
    client
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        porter::serve(&mut bridge, server),
    )
    .await
    .expect("slow history must leave the bridge loop immediately")
    .unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        read_line(&mut BufReader::new(client)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response["result"]["messages"], json!([]));
    assert_eq!(response["id"], "wire1");
    peer.abort();
}

fn history_request() -> Value {
    json!({"version":1,"method":"history","bridge_account":"aa".repeat(32),
        "session_key":agent().key(),"terminal_id":"term1","id":"history-boundary"})
}

fn history_export(bridge: &Bridge, slow: bool) {
    let parent = bridge.config.state_path.parent().unwrap();
    let wait = if slow {
        format!(
            "touch '{}'\nn=0; while [ ! -f '{}' ] && [ \"$n\" -lt 500 ]; do sleep 0.01; n=$((n+1)); done\n",
            parent.join("read-started").display(),
            parent.join("release-read").display()
        )
    } else {
        String::new()
    };
    std::fs::write(&bridge.config.opencode_bin,format!("#!/bin/sh\n{wait}printf '%s' '{{\"messages\":[{{\"info\":{{\"id\":\"room-input\",\"role\":\"user\"}},\"parts\":[{{\"type\":\"text\",\"text\":\"ROOM_HISTORY_SECRET\"}}]}}]}}'\n")).unwrap();
    std::fs::set_permissions(
        &bridge.config.opencode_bin,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}

fn other_transport_room_claim(bridge: &Bridge) {
    let mut other = bridge.config.clone();
    other.state_path = bridge.config.state_path.with_file_name("room-state.json");
    other.account_id_hex = "bb".repeat(32);
    let other = Bridge::open(other).unwrap();
    let root = other.config.native_claims_fixture_root.as_ref().unwrap();
    std::fs::DirBuilder::new().mode(0o700).create(root).unwrap();
    let native = ["opencode", "id", "ses_1"];
    let name = hex::encode(Sha256::digest(serde_json::to_vec(&native).unwrap()));
    // Exact durable-record fixture in a temporary shared root, not a user claim
    // or an account/session provisioning operation.
    let claim = json!({"version":1,"purpose":"room","native":native,
        "state":other.config.state_path.canonicalize().unwrap(),"owner":"11".repeat(32),
        "transport":other.config.account_id_hex,"group":"cc".repeat(16),
        "key":agent().key(),"terminal":"term1","pane":"w1:p1"});
    let path = root.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec(&claim).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn history_rejects_another_transports_room_claim_and_quarantined_binding() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    history_export(&bridge, false);
    other_transport_room_claim(&bridge);
    assert!(
        porter::handle(&mut bridge, history_request())
            .await
            .is_err(),
        "An old private desktop must not expose the other transport's room history"
    );
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .room_quarantined = Some("native_room_owned_elsewhere".into());
    bridge.save().unwrap();
    assert!(
        porter::handle(&mut bridge, history_request())
            .await
            .is_err()
    );
    peer.abort();
}

#[tokio::test]
async fn history_rejects_captured_in_memory_quarantine_without_waiting_for_save() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    history_export(&bridge, false);
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .room_quarantined = Some("already_held".into());
    assert!(
        porter::handle(&mut bridge, history_request())
            .await
            .is_err()
    );
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    client
        .write_all(format!("{}\n", history_request()).as_bytes())
        .await
        .unwrap();
    assert!(porter::serve(&mut bridge, server).await.is_err());
    peer.abort();
}

async fn spawned_history_change_during_read(claim: bool) {
    let _fixture = SPAWNED_READ_FIXTURE.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    history_export(&bridge, true);
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    client
        .write_all(format!("{}\n", history_request()).as_bytes())
        .await
        .unwrap();
    porter::serve(&mut bridge, server).await.unwrap();
    let started = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !dir.path().join("read-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    if started.is_ok() {
        if claim {
            other_transport_room_claim(&bridge);
        } else {
            bridge
                .state
                .mappings
                .get_mut(&agent().key())
                .unwrap()
                .room_quarantined = Some("held_during_read".into());
            bridge.save().unwrap();
        }
    }
    std::fs::write(dir.path().join("release-read"), b"").unwrap();
    started.expect("fixture reader must reach the slow-read boundary");
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        read_line(&mut BufReader::new(client)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response["error"]["code"], "porter_request_rejected");
    assert!(!response.to_string().contains("ROOM_HISTORY_SECRET"));
    peer.abort();
}

#[tokio::test]
async fn spawned_history_rechecks_native_claim_after_slow_read() {
    spawned_history_change_during_read(true).await;
}

#[tokio::test]
async fn spawned_history_rechecks_quarantine_after_slow_read() {
    spawned_history_change_during_read(false).await;
}

#[tokio::test]
async fn unmapped_private_history_without_a_room_claim_remains_available() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    bridge.state.mappings.clear();
    bridge.save().unwrap();
    history_export(&bridge, false);
    let response = porter::handle(&mut bridge, history_request())
        .await
        .unwrap();
    assert!(response.to_string().contains("ROOM_HISTORY_SECRET"));
    peer.abort();
}

#[tokio::test]
async fn socket_errors_are_bounded_and_do_not_disclose_reader_paths() {
    let _fixture = SPAWNED_READ_FIXTURE.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    let request = json!({"version":1,"id":"wire2","request_id":"request2","method":"history",
        "bridge_account":"aa".repeat(32),"session_key":agent().key(),"terminal_id":"term1"});
    client
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    porter::serve(&mut bridge, server).await.unwrap();
    let response = read_line(&mut BufReader::new(client)).await.unwrap();
    assert_eq!(response["error"]["code"], "porter_request_rejected");
    assert_eq!(response["request_id"], "request2");
    assert!(!response.to_string().contains(dir.path().to_str().unwrap()));
    peer.abort();
}

#[tokio::test]
async fn desktop_cannot_queue_phone_control_commands() {
    let dir = tempfile::tempdir().unwrap();
    let peer = host(dir.path()).await;
    let mut bridge = bridge(dir.path());
    for text in [
        "/clear-queue",
        "/queue",
        "/help",
        " /SyNc \n",
        "/status",
        "unmute",
        "/unmute",
        "/new idea idea opencode",
        " /NeW confirm n123 ",
        "/new",
        "/agent",
        "/agent p123 1",
        " /MoDeL confirm p123 ",
    ] {
        let mut request = enqueue();
        request["text"] = json!(text);
        assert!(
            porter::handle(&mut bridge, request).await.is_err(),
            "Command must not enter FIFO: {text}"
        );
    }
    assert!(
        bridge.state.mappings[&agent().key()]
            .prompt_queue
            .is_empty()
    );
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .is_empty()
    );
    peer.abort();
}

#[tokio::test]
async fn watch_pushes_only_changed_desktop_state_without_blocking_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = bridge(dir.path());
    let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
    client
        .write_all(b"{\"version\":1,\"method\":\"watch\"}\n")
        .await
        .unwrap();
    porter::serve(&mut bridge, server).await.unwrap();
    let mut client = BufReader::new(client);
    let ready = read_line(&mut client).await.unwrap();
    assert_eq!(ready["event"], "ready");
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .pending_prompt = true;
    bridge.save().unwrap();
    let changed = tokio::time::timeout(std::time::Duration::from_secs(1), read_line(&mut client))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed["event"], "changed");
    assert!(
        changed.get("text").is_none(),
        "notifications contain no transcript"
    );
    bridge.save().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), read_line(&mut client))
            .await
            .is_err(),
        "unchanged saved state must not generate another invalidation"
    );
}

#[tokio::test]
async fn stream_cli_multiplexes_requests_on_one_process_and_preserves_ids() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let _lock = herdr_white_noise_bridge::state::lock(&config.state_path).unwrap();
    let listener = porter::listen(&config.state_path).unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, format!(
        "phone_npub = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\naccount_id_hex = {:?}\nstate_path = {:?}\n",
        config.phone_npub, config.herdr_socket, config.wn_agent_socket, config.account_id_hex, config.state_path)).unwrap();
    let responder = tokio::spawn(async move {
        let (watch, _) = listener.accept().await.unwrap();
        let mut watch = BufReader::new(watch);
        assert_eq!(read_line(&mut watch).await.unwrap()["method"], "watch");
        watch
            .get_mut()
            .write_all(b"{\"version\":1,\"event\":\"ready\"}\n")
            .await
            .unwrap();
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            jobs.spawn(async move {
                let mut stream = BufReader::new(stream);
                let request = read_line(&mut stream).await.unwrap();
                if request["request_id"] == "durable-1" {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                stream
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"version":1,"id":request["id"],
                    "request_id":request["request_id"],"result":{"echo":request["request_id"]}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
        }
        while jobs.join_next().await.is_some() {}
        // Keep subscription alive until the command exits on stdin EOF.
        let _ = read_line(&mut watch).await;
    });
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "porter",
            "--stream",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(command.stdout.take().unwrap());
    let ready = read_line(&mut output).await;
    assert!(
        ready.is_ok(),
        "persistent CLI must start and acknowledge watch"
    );
    assert_eq!(ready.unwrap()["event"], "ready");
    let mut input = command.stdin.take().unwrap();
    for n in 1..=2 {
        input
            .write_all(
                format!(
                    "{}\n",
                    json!({"version":1,"method":"snapshot", "id":format!("wire-{n}"),
            "request_id":format!("durable-{n}")})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }
    // Closing stdin must drain already accepted requests before the CLI exits.
    drop(input);
    let second = read_line(&mut output).await.unwrap();
    let first = read_line(&mut output).await.unwrap();
    assert_eq!(second["id"], "wire-2");
    assert_eq!(first["id"], "wire-1");
    assert_eq!(first["result"]["echo"], "durable-1");
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), command.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    responder.abort();
}

#[tokio::test]
async fn cli_forwarding_works_while_the_bridge_owns_the_state_lock() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let _lock = herdr_white_noise_bridge::state::lock(&config.state_path).unwrap();
    let listener = porter::listen(&config.state_path).unwrap();
    let config_path = dir.path().join("config.toml");
    let text = format!(
        "phone_npub = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\naccount_id_hex = {:?}\nstate_path = {:?}\n",
        config.phone_npub,
        config.herdr_socket,
        config.wn_agent_socket,
        config.account_id_hex,
        config.state_path
    );
    std::fs::write(&config_path, text).unwrap();
    let responder = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        assert_eq!(request["method"], "snapshot");
        let response = json!({"id":request["id"],"request_id":request["request_id"],"version":1,"result":{"sessions":[]}});
        stream
            .get_mut()
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
    });
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args(["--config", config_path.to_str().unwrap(), "porter"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    command
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"version\":1,\"method\":\"snapshot\",\"request_id\":\"r1\"}\n")
        .await
        .unwrap();
    let output = command.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["version"], 1);
    assert_eq!(response["request_id"], "r1");
    assert_eq!(response["result"]["sessions"], json!([]));
    responder.await.unwrap();
}

#[tokio::test]
async fn desktop_endpoint_is_disabled_unless_explicitly_enabled() {
    for (setting, arguments, enabled) in [
        (None, &[][..], false),
        (None, &["run"][..], false),
        (Some(false), &["run"][..], false),
        (Some(true), &["run"][..], true),
        (Some(false), &["run", "--portr"][..], true),
        (None, &["run", "--portr"][..], true),
        (None, &["run", "--porter"][..], true),
        (Some(true), &["run", "--no-portr"][..], false),
        (Some(true), &["run", "--no-porter"][..], false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (peer, ready) = host_ready(dir.path()).await;
        let bridge = bridge(dir.path());
        let config = bridge.config.clone();
        drop(bridge);
        let config_root = dir.path().join(".config/herdr-white-noise-bridge");
        std::fs::create_dir_all(&config_root).unwrap();
        let mut text = format!(
            "phone_npub = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\naccount_id_hex = {:?}\nstate_path = {:?}\n",
            config.phone_npub,
            config.herdr_socket,
            config.wn_agent_socket,
            config.account_id_hex,
            config.state_path
        );
        if let Some(setting) = setting {
            text.push_str(&format!("portr_enabled = {setting}\n"));
        }
        std::fs::write(config_root.join("config.toml"), text).unwrap();
        let mut worker =
            tokio::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
                .args(arguments)
                .env("HOME", dir.path())
                .env_remove("XDG_CONFIG_HOME")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), ready)
            .await
            .expect("Worker must reach the phone session reconciliation loop")
            .unwrap();
        assert!(worker.try_wait().unwrap().is_none());
        assert_eq!(
            porter::socket_path(&config.state_path).exists(),
            enabled,
            "setting={setting:?}, arguments={arguments:?}"
        );
        if enabled {
            let mut stream = BufReader::new(
                tokio::net::UnixStream::connect(porter::socket_path(&config.state_path))
                    .await
                    .unwrap(),
            );
            stream.get_mut().write_all(b"{\"version\":1,\"method\":\"snapshot\",\"id\":\"wire-default\",\"request_id\":\"client-default\"}\n").await.unwrap();
            let response = read_line(&mut stream).await.unwrap();
            assert_eq!(response["result"]["bridge_account"], config.account_id_hex);
        }
        worker.kill().await.unwrap();
        peer.abort();
    }
}

#[test]
fn desktop_enable_and_disable_flags_conflict_before_startup() {
    for flags in [["--portr", "--no-portr"], ["--porter", "--no-porter"]] {
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
            .arg("--config")
            .arg(root.path().join("missing.toml"))
            .arg("run")
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
