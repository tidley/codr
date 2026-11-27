use agent_connector::{MarmotNostrSigner, external_signer::ExternalSigner, nostr::prelude::*};
use herdr_white_noise_bridge::wire::read_line;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
};

fn keys() -> Keys {
    Keys::parse(&"11".repeat(32)).unwrap()
}
fn listener(root: &std::path::Path) -> UnixListener {
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.join("signer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    listener
}
async fn reply(listener: UnixListener, result: Value) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let mut stream = BufReader::new(stream);
    let request = read_line(&mut stream).await.unwrap();
    let response = json!({"version":1,"id":request["id"],"result":result});
    stream
        .get_mut()
        .write_all(format!("{response}\n").as_bytes())
        .await
        .unwrap();
    request
}

#[tokio::test]
async fn external_human_identity_is_pinned_without_a_secret_request() {
    for changed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let listener = listener(root.path());
        let key = if changed {
            Keys::generate().public_key()
        } else {
            keys().public_key()
        };
        let task = tokio::spawn(reply(listener, json!(key.to_hex())));
        let signer = ExternalSigner::new(root.path().join("signer.sock"), keys().public_key());
        assert_eq!(signer.get_public_key().await.is_ok(), !changed);
        let request = task.await.unwrap();
        assert_eq!(request["method"], "get_public_key");
        assert_eq!(request["params"], json!([]));
    }
}

#[tokio::test]
async fn external_human_signatures_must_bind_the_exact_event_and_account() {
    for substitute in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let listener = listener(root.path());
        let event = EventBuilder::new(Kind::TextNote, "fixture only")
            .finalize(&keys())
            .unwrap();
        let unsigned: UnsignedEvent =
            serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
        let returned = if substitute {
            EventBuilder::new(Kind::TextNote, "substituted")
                .finalize(&keys())
                .unwrap()
        } else {
            event
        };
        let task = tokio::spawn(reply(listener, serde_json::to_value(returned).unwrap()));
        let signer = ExternalSigner::new(root.path().join("signer.sock"), keys().public_key());
        assert_eq!(signer.sign_event(unsigned).await.is_ok(), !substitute);
        assert_eq!(task.await.unwrap()["method"], "sign_event");
    }
}

#[tokio::test]
async fn external_human_refusal_unavailable_and_bad_socket_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let listener = listener(root.path());
    let signer = ExternalSigner::new(root.path().join("signer.sock"), keys().public_key());
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        stream
            .get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"version":1,"id":request["id"],"error":"PRIVATE SIGNER REFUSAL"})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let error = signer.get_public_key().await.unwrap_err().to_string();
    assert!(!error.contains("PRIVATE SIGNER REFUSAL"));
    task.await.unwrap();
    assert!(signer.get_public_key().await.is_err());
    std::fs::set_permissions(
        root.path().join("signer.sock"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(signer.get_public_key().await.is_err());
}

#[tokio::test]
async fn external_human_signer_cancellation_never_retries_or_falls_back() {
    let root = tempfile::tempdir().unwrap();
    let listener = listener(root.path());
    let signer = ExternalSigner::new(root.path().join("signer.sock"), keys().public_key());
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        // No reply: keep the only fixture connection alive until cancellation.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        request
    });
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            signer.get_public_key()
        )
        .await
        .is_err()
    );
    assert_eq!(task.await.unwrap()["method"], "get_public_key");
}

#[tokio::test]
async fn external_human_invalid_frames_and_signatures_fail_closed() {
    for mode in [
        "wrong_id",
        "wrong_version",
        "malformed",
        "oversized",
        "bad_signature",
    ] {
        let root = tempfile::tempdir().unwrap();
        let listener = listener(root.path());
        let signer = ExternalSigner::new(root.path().join("signer.sock"), keys().public_key());
        let event = EventBuilder::new(Kind::TextNote, "fixture only")
            .finalize(&keys())
            .unwrap();
        let unsigned: UnsignedEvent =
            serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
        let mut invalid_event = serde_json::to_value(event).unwrap();
        invalid_event["sig"] = json!("00".repeat(64));
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let request = read_line(&mut stream).await.unwrap();
            let response = match mode {
                "wrong_id" => format!(
                    "{}\n",
                    json!({"version":1,"id":"other","result":keys().public_key().to_hex()})
                ),
                "wrong_version" => format!(
                    "{}\n",
                    json!({"version":2,"id":request["id"],"result":keys().public_key().to_hex()})
                ),
                "malformed" => "not json\n".into(),
                // The pinned agent-control frame limit is one MiB.
                "oversized" => format!("{}\n", "x".repeat(1024 * 1024 + 1)),
                _ => format!(
                    "{}\n",
                    json!({"version":1,"id":request["id"],"result":invalid_event})
                ),
            };
            // An oversized reply may be closed by the bounded reader mid-write.
            let _ = stream.get_mut().write_all(response.as_bytes()).await;
        });
        if mode == "bad_signature" {
            assert!(signer.sign_event(unsigned).await.is_err());
        } else {
            assert!(signer.get_public_key().await.is_err());
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn external_human_actual_signer_timeout_is_bounded_and_has_no_retry() {
    let root = tempfile::tempdir().unwrap();
    let listener = listener(root.path());
    let signer = ExternalSigner::new(root.path().join("signer.sock"), keys().public_key());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let request = read_line(&mut stream).await.unwrap();
        // Keep the response connection open beyond the production timeout.
        tokio::time::sleep(std::time::Duration::from_secs(121)).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        request
    });
    let started = std::time::Instant::now();
    assert!(
        signer
            .get_public_key()
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(125));
    assert_eq!(server.await.unwrap()["method"], "get_public_key");
}
