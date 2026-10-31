use bech32::ToBase32;
use herdr_white_noise_bridge::{BridgeConfig, messaging::EmbeddedMessaging, wire::Socket};
use serde_json::json;
use std::process::Command;
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    time::Duration,
};

fn config(root: &Path) -> BridgeConfig {
    serde_json::from_value(json!({
        "phone_npub":bech32::encode("npub",[0x11u8;32].to_base32(),bech32::Variant::Bech32).unwrap(),
        "herdr_socket":root.join("herdr.sock"),"wn_agent_socket":root.join("mdk/dev/control.sock"),
        "state_path":root.join("state.json"),"account_id_hex":"aa".repeat(32)
    })).unwrap()
}

#[tokio::test]
async fn combined_runtime_serves_without_creating_an_account_and_releases_its_home() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("mdk");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let config = config(&root);
    for _ in 0..2 {
        let runtime = EmbeddedMessaging::start(&config, home.clone())
            .await
            .unwrap();
        let socket = Socket {
            path: config.wn_agent_socket.clone(),
        };
        let response = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(value) = socket.marmot(json!({"type":"account_list"})).await {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(response["type"], "account_list");
        assert_eq!(response["accounts"], json!([]));
        assert_eq!(
            std::fs::metadata(&config.wn_agent_socket)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        tokio::time::timeout(Duration::from_secs(15), runtime.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn combined_runtime_refuses_an_external_socket_owner_without_touching_accounts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("mdk");
    std::fs::create_dir_all(home.join("dev")).unwrap();
    let config = config(&root);
    let _listener = tokio::net::UnixListener::bind(&config.wn_agent_socket).unwrap();
    assert!(
        EmbeddedMessaging::start(&config, home.clone())
            .await
            .is_err()
    );
    assert!(!home.join("shared.sqlite3").exists());
}

#[tokio::test]
async fn combined_runtime_rejects_relative_or_missing_account_homes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let config = config(&root);
    assert!(
        EmbeddedMessaging::start(&config, "relative".into())
            .await
            .is_err()
    );
    assert!(
        EmbeddedMessaging::start(&config, root.join("missing"))
            .await
            .is_err()
    );
    assert!(!root.join("missing").exists());
}

#[tokio::test]
async fn combined_runtime_keeps_desktop_reads_available_after_messaging_startup_failure() {
    use tokio::io::AsyncWriteExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let settings = config(&root);
    let home = root.join("mdk");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    std::fs::write(home.join("shared.sqlite3"), b"invalid fixture database").unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path,format!("phone_npub = {:?}\naccount_id_hex = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\nstate_path = {:?}\n",settings.phone_npub,settings.account_id_hex,settings.herdr_socket,settings.wn_agent_socket,settings.state_path)).unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
            .arg("--config")
            .arg(path)
            .args(["run", "--portr", "--messaging-home"])
            .arg(&home)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(child.0.try_wait().unwrap().is_none());
            if let Ok(mut socket) = tokio::net::UnixStream::connect(
                herdr_white_noise_bridge::porter::socket_path(&settings.state_path),
            )
            .await
            {
                socket
                    .write_all(b"{\"version\":1,\"method\":\"watch\"}\n")
                    .await
                    .unwrap();
                let response = herdr_white_noise_bridge::wire::read_line(
                    &mut tokio::io::BufReader::new(socket),
                )
                .await
                .unwrap();
                assert_eq!(response["event"], "ready");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(child.0.try_wait().unwrap().is_none());
}

#[tokio::test]
async fn combined_process_sigterm_releases_the_messaging_home() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let settings = config(&root);
    let home = root.join("mdk");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path,format!("phone_npub = {:?}\naccount_id_hex = {:?}\nherdr_socket = {:?}\nwn_agent_socket = {:?}\nstate_path = {:?}\n",settings.phone_npub,settings.account_id_hex,settings.herdr_socket,settings.wn_agent_socket,settings.state_path)).unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
            .arg("--config")
            .arg(path)
            .args(["run", "--messaging-home"])
            .arg(&home)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = Socket {
        path: settings.wn_agent_socket.clone(),
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(child.0.try_wait().unwrap().is_none());
            if socket.marmot(json!({"type":"account_list"})).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let runtime = EmbeddedMessaging::start(&settings, home).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), runtime.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn combined_runtime_is_an_explicit_same_binary_run_option() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .args(["run", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--messaging-home"));
}
