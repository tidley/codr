use herdr_white_noise_bridge::{BridgeConfig, prompt};
use serde_json::{Value, json};
use std::io::{Read, Write};

fn config(root: &std::path::Path, url: &str) -> BridgeConfig {
    let file = root.join("service.json");
    std::fs::write(
        &file,
        json!({"url":url,"pid":1,"password":"fixture"}).to_string(),
    )
    .unwrap();
    let mut value: toml::Value = toml::from_str(include_str!("../config.example.toml")).unwrap();
    value.as_table_mut().unwrap().insert(
        "opencode_service_file".into(),
        file.to_string_lossy().to_string().into(),
    );
    let mut config: BridgeConfig = value.try_into().unwrap();
    config.native_claims_fixture_root = Some(root.canonicalize().unwrap().join("native-claims"));
    config
}

fn request(socket: &mut std::net::TcpStream) -> (String, Value) {
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let headers = String::from_utf8(bytes.clone()).unwrap();
    assert!(headers.to_lowercase().contains("authorization: basic "));
    let length = headers
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    bytes.resize(header_end + length, 0);
    socket.read_exact(&mut bytes[header_end..]).unwrap();
    (
        headers.lines().next().unwrap().to_string(),
        if length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&bytes[header_end..]).unwrap()
        },
    )
}

fn response(socket: &mut std::net::TcpStream, status: &str, body: Value) {
    let body = body.to_string();
    write!(
        socket,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

#[tokio::test]
async fn sends_once_to_the_exact_session_and_validates_the_native_receipt() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(
        root.path(),
        &format!("http://{}", listener.local_addr().unwrap()),
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let (path, body) = request(&mut socket);
        assert_eq!(path, "POST /api/session/ses_fixture/prompt HTTP/1.1");
        assert_eq!(
            body,
            json!({"id":"msg_codr_fixture","text":"hello","delivery":"steer"})
        );
        response(
            &mut socket,
            "200 OK",
            json!({"data":{"id":"msg_codr_fixture","sessionID":"ses_fixture","type":"user","payload":{"text":"hello"}}}),
        );
    });
    let accepted = prompt::send(&config, "ses_fixture", "msg_codr_fixture", "hello")
        .await
        .unwrap();
    assert_eq!(accepted.id, "opencode:msg_codr_fixture");
    server.join().unwrap();
}

#[tokio::test]
async fn a_lost_response_never_retries_the_post() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(
        root.path(),
        &format!("http://{}", listener.local_addr().unwrap()),
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert!(request(&mut socket).0.starts_with("POST "));
        drop(socket);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            listener.accept().is_err(),
            "No second request after a lost acknowledgement"
        );
    });
    assert!(
        prompt::send(&config, "ses_fixture", "msg_codr_fixture", "hello")
            .await
            .is_err()
    );
    server.join().unwrap();
}

#[tokio::test]
async fn restart_readback_finds_an_accepted_but_not_yet_delivered_inbox_item() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(
        root.path(),
        &format!("http://{}", listener.local_addr().unwrap()),
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(
            request(&mut socket).0,
            "GET /api/session/ses_fixture/message/msg_codr_fixture HTTP/1.1"
        );
        response(&mut socket, "404 Not Found", json!({}));
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(
            request(&mut socket).0,
            "GET /api/session/ses_fixture/inbox HTTP/1.1"
        );
        response(
            &mut socket,
            "200 OK",
            json!({"data":[{"id":"msg_codr_fixture","sessionID":"ses_fixture","type":"user","payload":{"text":"hello"}}]}),
        );
    });
    let accepted = prompt::accepted(&config, "ses_fixture", "msg_codr_fixture")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.id, "opencode:msg_codr_fixture");
    server.join().unwrap();
}

#[tokio::test]
async fn rejects_remote_service_and_path_injection_before_any_post() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path(), "http://example.com");
    assert!(
        prompt::send(&config, "ses_fixture", "msg_codr_fixture", "hello")
            .await
            .is_err()
    );
    assert!(
        prompt::send(&config, "ses_fixture/other", "msg_codr_fixture", "hello")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn mismatched_receipts_and_http_failures_remain_uncertain() {
    for field in ["id", "sessionID", "type", "text", "http"] {
        let root = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = config(
            root.path(),
            &format!("http://{}", listener.local_addr().unwrap()),
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            let mut value = json!({"data":{"id":"msg_codr_fixture","sessionID":"ses_fixture","type":"user","payload":{"text":"hello"}}});
            if field == "text" {
                value["data"]["payload"]["text"] = json!("different");
            } else if field != "http" {
                value["data"][field] = json!("different");
            }
            response(
                &mut socket,
                if field == "http" {
                    "503 Unavailable"
                } else {
                    "200 OK"
                },
                value,
            );
        });
        let error = prompt::send(&config, "ses_fixture", "msg_codr_fixture", "hello")
            .await
            .unwrap_err();
        assert!(!error.is::<herdr_white_noise_bridge::wire::NotSubmitted>());
        server.join().unwrap();
    }
}

#[tokio::test]
async fn readback_uses_the_saved_message_id_without_a_post() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(
        root.path(),
        &format!("http://{}", listener.local_addr().unwrap()),
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert!(
            request(&mut socket)
                .0
                .starts_with("GET /api/session/ses_fixture/message/msg_codr_fixture ")
        );
        response(
            &mut socket,
            "200 OK",
            json!({"data":{"id":"msg_codr_fixture","type":"user","text":"hello"}}),
        );
    });
    assert!(
        prompt::accepted(&config, "ses_fixture", "msg_codr_fixture")
            .await
            .unwrap()
            .is_some()
    );
    server.join().unwrap();
}

#[test]
fn codex_claude_and_legacy_opencode_keep_terminal_routing() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path(), "http://127.0.0.1:1");
    for kind in ["codex", "claude", "opencode"] {
        let agent = serde_json::from_value(json!({"pane_id":"w1:p1","terminal_id":"t1","tab_id":"w1:t1","agent":kind,"agent_status":"idle","agent_session":{"source":"test","agent":kind,"kind":"id","value":"ses_fixture"}})).unwrap();
        assert_eq!(
            prompt::native_session(&config, &agent).is_some(),
            kind == "opencode"
        );
        if kind == "opencode" {
            config.opencode_service_file = None;
            assert!(prompt::native_session(&config, &agent).is_none());
        }
    }
}

/// Opt-in release probe. The session must be a dedicated scratch validation
/// session, never an existing user conversation. One invocation sends one POST.
#[tokio::test]
#[ignore = "requires an explicitly prepared scratch session on the live OpenCode service"]
async fn live_service_accepts_one_explicit_validation_prompt() {
    let session = std::env::var("CODR_LIVE_SESSION").expect("Explicit scratch session required");
    assert!(session.starts_with("ses_codr_release_validation_"));
    let config = BridgeConfig::load(&std::path::PathBuf::from(
        std::env::var("CODR_LIVE_CONFIG").expect("Explicit service config required"),
    ))
    .unwrap();
    let text = std::fs::read_to_string(std::env::var("CODR_LIVE_TEXT").unwrap()).unwrap();
    let receipt_file = std::path::PathBuf::from(std::env::var("CODR_LIVE_RECEIPT").unwrap());
    let id = format!("msg_codr_{}", uuid::Uuid::new_v4().simple());
    let mut submission =
        herdr_white_noise_bridge::activity::Submission::new("release-validation", &text, None, 0);
    submission.native_message_id = Some(id.clone());
    let bytes = serde_json::to_vec(&submission).unwrap();
    // create_new prevents accidental reuse of a previous validation attempt.
    let mut receipt = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&receipt_file)
        .unwrap();
    receipt.write_all(&bytes).unwrap();
    receipt.sync_all().unwrap();
    let accepted = prompt::send(&config, &session, &id, &text).await.unwrap();
    let mut pending = vec![submission];
    herdr_white_noise_bridge::activity::Submission::observe(&mut pending, &[accepted]);
    assert!(
        pending.is_empty(),
        "Live native acceptance must match ID and text"
    );
}
