use bech32::ToBase32;
use herdr_white_noise_bridge::{BridgeConfig, bridge::Bridge, state::Mapping, wire::read_line};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{io::AsyncWriteExt, net::UnixListener};

const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[tokio::test]
async fn bare_new_begins_a_durable_workspace_prompt_without_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    let mut bridge = bridge(&config);
    bridge.inbound(&inbound("begin", "/new")).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("Existing workspaces are unavailable")
    );
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("Choose workspace")
    );
    assert!(
        !bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("Reply with a listed number.")
    );
    let text = bridge.state.outbox.last().unwrap().text.clone();
    drop(bridge);
    let mut restarted = Bridge::open(config.clone()).unwrap();
    restarted.inbound(&inbound("begin", "/new")).await.unwrap();
    assert_eq!(restarted.state.outbox.last().unwrap().text, text);
    assert_eq!(std::fs::read_dir(&config.launch_root).unwrap().count(), 0);
}

#[test]
fn launch_root_ancestors_cannot_be_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir_all(real.join("projects")).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let configured = link.join("projects");
    assert!(
        herdr_white_noise_bridge::launch::proposal(
            "/new idea idea opencode",
            &configured,
            root.path(),
            0
        )
        .is_err()
    );
    assert!(!real.join("projects/idea").exists());
}
fn config(root: &Path) -> BridgeConfig {
    // tempfile's macOS root sits behind the /var symlink; launch roots must be real paths.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    serde_json::from_value(json!({
        "phone_npub":bech32::encode("npub",[0x11u8;32].to_base32(),bech32::Variant::Bech32).unwrap(),
        "account_id_hex":"aa".repeat(32),"herdr_socket":root.join("herdr.sock"),
        "wn_agent_socket":root.join("wn.sock"),"state_path":root.join("state.json"),
        "launch_root":root.join("projects"),"auto_forwarding":false
    })).unwrap()
}
fn inbound(id: &str, text: &str) -> Value {
    json!({"marmot_agent_control":"marmot.agent-control.v2","type":"inbound_message",
        "account_id_hex":"aa".repeat(32),"group_id_hex":"bb".repeat(16),
        "message":{"message_id_hex":id,"sender":{"account_id_hex":OWNER},"text":text}})
}
fn bridge(config: &BridgeConfig) -> Bridge {
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.state.mappings.insert(
        "source".into(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            active: false,
            ..Mapping::default()
        },
    );
    bridge.save().unwrap();
    bridge
}
fn code(bridge: &Bridge) -> String {
    bridge.state.mappings["source"]
        .launch
        .as_ref()
        .expect("Explicit confirmation request")
        .code
        .clone()
}
async fn host(config: &BridgeConfig, calls: Arc<Mutex<Vec<Value>>>) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(&config.herdr_socket).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let request = read_line(&mut reader).await.unwrap();
            calls.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "workspace.create" => {
                    json!({"type":"workspace_created","workspace":{"workspace_id":"w99"},"root_pane":{"pane_id":"w99:p1","terminal_id":"term_new"}})
                }
                "agent.start" => {
                    json!({"type":"agent_started","agent":{"name":request["params"]["name"],"pane_id":"w99:p1","terminal_id":"term_new","workspace_id":"w99"}})
                }
                method => panic!("Unexpected mutation or shell call: {method}"),
            };
            reader
                .get_mut()
                .write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes())
                .await
                .unwrap();
        }
    })
}

#[tokio::test]
async fn preview_requires_confirmation_before_creating_a_folder_or_starting_an_agent() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    let target = config.launch_root.join("new-idea");
    let mut bridge = bridge(&config);
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let request = inbound(
        "proposal",
        &format!("/new my-idea {} opencode", target.display()),
    );
    bridge.inbound(&request).await.unwrap();
    assert!(!target.exists());
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("Folder will be created")
    );
    assert!(!bridge.state.forwarding_enabled);
    let token = code(&bridge);
    bridge.inbound(&request).await.unwrap();
    assert_eq!(code(&bridge), token);
    let calls = Arc::new(Mutex::new(vec![]));
    let server = host(&config, calls.clone()).await;
    let confirmation = inbound("confirm", &format!("/new confirm {token}"));
    bridge.inbound(&confirmation).await.unwrap();
    assert!(target.is_dir());
    assert!(!bridge.state.forwarding_enabled);
    drop(bridge);
    let mut restarted = Bridge::open(config.clone()).unwrap();
    restarted.inbound(&confirmation).await.unwrap();
    restarted
        .inbound(&inbound("confirm-again", &format!("/new confirm {token}")))
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["method"], "workspace.create");
    assert_eq!(calls[0]["params"]["cwd"], target.to_str().unwrap());
    assert_eq!(calls[0]["params"]["label"], "my-idea");
    assert_eq!(calls[0]["params"]["focus"], false);
    assert_eq!(calls[1]["method"], "agent.start");
    assert_eq!(calls[1]["params"]["kind"], "opencode");
    assert_eq!(calls[1]["params"]["pane_id"], "w99:p1");
    assert_eq!(calls[1]["params"]["args"], json!([]));
    server.abort();
}

#[tokio::test]
async fn existing_folder_is_reported_and_kept_after_confirmation_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let target = config.launch_root.join("existing");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "original project").unwrap();
    let mut bridge = bridge(&config);
    bridge
        .inbound(&inbound("preview", "/new existing existing claude"))
        .await
        .unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("Folder already exists")
    );
    let token = code(&bridge);
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let server = host(&config, calls.clone()).await;
    bridge
        .inbound(&inbound("confirm", &format!("/new confirm {token}")))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt")).unwrap(),
        "original project"
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(calls.lock().unwrap()[1]["params"]["kind"], "claude");
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("existing folder and its contents were kept")
    );
    server.abort();
}

#[tokio::test]
async fn a_folder_that_appears_after_preview_requires_a_new_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    let target = config.launch_root.join("idea");
    let mut bridge = bridge(&config);
    bridge
        .inbound(&inbound("preview", "/new idea idea codex"))
        .await
        .unwrap();
    let old = code(&bridge);
    std::fs::create_dir(&target).unwrap();
    bridge
        .inbound(&inbound("first-confirm", &format!("/new confirm {old}")))
        .await
        .unwrap();
    let new = code(&bridge);
    assert_ne!(old, new);
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("Folder already exists")
    );
    let calls = Arc::new(Mutex::new(vec![]));
    let server = host(&config, calls.clone()).await;
    bridge
        .inbound(&inbound("stale-confirm", &format!("/new confirm {old}")))
        .await
        .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    bridge
        .inbound(&inbound("new-confirm", &format!("/new confirm {new}")))
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn cancelling_expired_or_foreign_confirmations_never_creates_a_folder() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    let mut bridge = bridge(&config);
    bridge
        .inbound(&inbound("preview", "/new idea idea opencode"))
        .await
        .unwrap();
    let token = code(&bridge);
    let mut foreign = inbound("foreign", &format!("/new confirm {token}"));
    foreign["message"]["sender"]["account_id_hex"] = json!("99".repeat(32));
    bridge.inbound(&foreign).await.unwrap();
    bridge.state.mappings.insert(
        "other".into(),
        Mapping {
            group_id_hex: Some("dd".repeat(16)),
            ..Mapping::default()
        },
    );
    let mut other = inbound("other-chat", &format!("/new confirm {token}"));
    other["group_id_hex"] = json!("dd".repeat(16));
    bridge.inbound(&other).await.unwrap();
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("No matching launch code")
    );
    bridge
        .inbound(&inbound("cancel", &format!("/new cancel {token}")))
        .await
        .unwrap();
    bridge
        .inbound(&inbound("after-cancel", &format!("/new confirm {token}")))
        .await
        .unwrap();
    assert!(!config.launch_root.join("idea").exists());
    bridge
        .inbound(&inbound("second-preview", "/new idea idea opencode"))
        .await
        .unwrap();
    let token = code(&bridge);
    bridge
        .state
        .mappings
        .get_mut("source")
        .unwrap()
        .launch
        .as_mut()
        .unwrap()
        .expires_at = 0;
    bridge
        .inbound(&inbound("expired", &format!("/new confirm {token}")))
        .await
        .unwrap();
    assert!(!config.launch_root.join("idea").exists());
    assert!(bridge.state.outbox.last().unwrap().text.contains("expired"));
}

#[tokio::test]
async fn malformed_paths_captions_and_truncation_cannot_launch() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    symlink(root.path(), config.launch_root.join("escape")).unwrap();
    std::fs::write(config.launch_root.join("file"), "keep").unwrap();
    let mut bridge = bridge(&config);
    for (i, text) in [
        "/new idea ../outside opencode",
        "/new idea escape/child opencode",
        "/new idea file opencode",
        "/new idea .git opencode",
        "/new idea idea sh",
        "/new idea idea opencode --command=bad",
        "/new idea /etc opencode",
        "/new idea foo;touch opencode",
        "/new bad;name idea opencode",
    ]
    .iter()
    .enumerate()
    {
        bridge
            .inbound(&inbound(&format!("bad-{i}"), text))
            .await
            .unwrap();
        assert!(bridge.state.mappings["source"].launch.is_none(), "{text}");
    }
    for field in ["text_truncated", "attachments_truncated", "media"] {
        let mut event = inbound(field, "/new idea idea opencode");
        event["message"][field] = if field == "media" {
            json!([{"media_type":"image/png"}])
        } else {
            json!(true)
        };
        bridge.inbound(&event).await.unwrap();
        assert!(bridge.state.mappings["source"].launch.is_none());
    }
    assert!(!config.launch_root.join("idea").exists());
    assert!(!root.path().join("outside").exists());
}

#[tokio::test]
async fn replacing_an_existing_directory_requires_confirmation_again() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let target = config.launch_root.join("idea");
    std::fs::create_dir_all(&target).unwrap();
    let mut bridge = bridge(&config);
    bridge
        .inbound(&inbound("preview", "/new idea idea opencode"))
        .await
        .unwrap();
    let token = code(&bridge);
    std::fs::rename(&target, config.launch_root.join("original")).unwrap();
    std::fs::create_dir(&target).unwrap();
    bridge
        .inbound(&inbound("confirm", &format!("/new confirm {token}")))
        .await
        .unwrap();
    assert_ne!(token, code(&bridge));
    assert!(
        bridge
            .state
            .outbox
            .last()
            .unwrap()
            .text
            .contains("folder changed")
    );
}

#[tokio::test]
async fn lost_workspace_or_agent_acknowledgements_are_never_retried() {
    for drop_agent in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        std::fs::create_dir(&config.launch_root).unwrap();
        let mut bridge = bridge(&config);
        bridge
            .inbound(&inbound("preview", "/new idea idea opencode"))
            .await
            .unwrap();
        let token = code(&bridge);
        let listener = UnixListener::bind(&config.herdr_socket).unwrap();
        let calls = Arc::new(Mutex::new(vec![]));
        let seen = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let request = read_line(&mut reader).await.unwrap();
                seen.lock().unwrap().push(request.clone());
                if !drop_agent || request["method"] == "agent.start" {
                    break;
                }
                let response = json!({"id":request["id"],"result":{"type":"workspace_created","workspace":{"workspace_id":"w99"},"root_pane":{"pane_id":"w99:p1","terminal_id":"term_new"}}});
                reader
                    .get_mut()
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        bridge
            .inbound(&inbound("confirm", &format!("/new confirm {token}")))
            .await
            .unwrap();
        server.await.unwrap();
        assert!(config.launch_root.join("idea").is_dir());
        assert_eq!(
            bridge.state.mappings["source"]
                .launch
                .as_ref()
                .unwrap()
                .phase,
            herdr_white_noise_bridge::launch::Phase::Uncertain
        );
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge
            .inbound(&inbound("confirm-again", &format!("/new confirm {token}")))
            .await
            .unwrap();
        bridge
            .inbound(&inbound("new-request", "/new another another codex"))
            .await
            .unwrap();
        assert!(!config.launch_root.join("another").exists());
        assert_eq!(calls.lock().unwrap().len(), if drop_agent { 2 } else { 1 });
        assert!(
            bridge
                .state
                .outbox
                .last()
                .unwrap()
                .text
                .contains("local inspection")
        );
    }
}

#[tokio::test]
async fn a_failed_intent_save_prevents_all_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    let mut bridge = bridge(&config);
    bridge
        .inbound(&inbound("preview", "/new idea idea opencode"))
        .await
        .unwrap();
    let token = code(&bridge);
    std::fs::rename(&config.state_path, root.path().join("saved-state.json")).unwrap();
    std::fs::create_dir(&config.state_path).unwrap();
    assert!(
        bridge
            .inbound(&inbound("confirm", &format!("/new confirm {token}")))
            .await
            .is_err()
    );
    assert!(!config.launch_root.join("idea").exists());
    assert_eq!(
        bridge.state.mappings["source"]
            .launch
            .as_ref()
            .unwrap()
            .phase,
        herdr_white_noise_bridge::launch::Phase::Awaiting
    );
}

#[tokio::test]
async fn confirmation_with_attachments_or_truncation_never_starts() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir(&config.launch_root).unwrap();
    let mut bridge = bridge(&config);
    bridge
        .inbound(&inbound("preview", "/new idea idea opencode"))
        .await
        .unwrap();
    let token = code(&bridge);
    for field in [
        "attachments",
        "media",
        "text_truncated",
        "attachments_truncated",
    ] {
        let mut event = inbound(field, &format!("/new confirm {token}"));
        event["message"][field] = if field.ends_with("truncated") {
            json!(true)
        } else {
            json!([{"media_type":"image/png"}])
        };
        bridge.inbound(&event).await.unwrap();
        assert_eq!(
            bridge.state.mappings["source"]
                .launch
                .as_ref()
                .unwrap()
                .phase,
            herdr_white_noise_bridge::launch::Phase::Awaiting
        );
        assert!(!config.launch_root.join("idea").exists());
    }
}
