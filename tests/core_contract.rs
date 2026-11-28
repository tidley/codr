//! No server, model or discoverable agent is started by these contract fixtures.
use herdr_white_noise_bridge::{herdr, wire::Socket};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("fixtures/herdr-core-0.1.22/schema.json")).unwrap()
}

#[test]
fn public_reference_fixtures_have_exact_pinned_bytes() {
    use sha2::{Digest, Sha256};
    for (bytes, digest) in [
        (
            include_bytes!("fixtures/herdr-core-0.1.22/schema.json").as_slice(),
            "e4f018b6b7b11f9b18a1a6bc8d6fb48c89d57e67c930f11bca8da6a068abeeb4",
        ),
        (
            include_bytes!("fixtures/herdr-core-0.1.22/protocol.md").as_slice(),
            "669240f145352b8ef8069b2679c23581d14d1917f06688e4b2bfedd45599ad1f",
        ),
        (
            include_bytes!("fixtures/herdr-core-0.1.22/app-commands.md").as_slice(),
            "3609ca249a10e1cc0d365c83ae6fe7d87463df43a199f1dcc71e3b92e1571cbb",
        ),
    ] {
        assert_eq!(hex::encode(Sha256::digest(bytes)), digest);
    }
}

async fn fake(
    replies: Vec<(&'static str, Value)>,
) -> (tempfile::TempDir, Socket, tokio::task::JoinHandle<()>) {
    let root = tempfile::tempdir().unwrap();
    let socket = Socket {
        path: root.path().join("hc.sock"),
    };
    let listener = UnixListener::bind(&socket.path).unwrap();
    let task = tokio::spawn(async move {
        for (method, result) in replies {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            if method == "session.snapshot" {
                assert!(
                    request.get("jsonrpc").is_none(),
                    "Preserve the legacy request envelope"
                );
            } else {
                assert_eq!(request["jsonrpc"], "2.0");
            }
            assert_eq!(request["method"], method);
            let response = if let Some(envelope) = result.get("_envelope") {
                let mut envelope = envelope.clone();
                envelope["id"] = request["id"].clone();
                envelope
            } else if let Some(error) = result.get("_error") {
                json!({"jsonrpc":"2.0","id":request["id"],"error":error})
            } else {
                json!({"jsonrpc":"2.0","id":request["id"],"result":result})
            };
            reader
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    (root, socket, task)
}

fn absent() -> Value {
    json!({"_error":{"code":-32601,"message":"Method not found"}})
}

fn terminal(address: &str, session: &str) -> Value {
    json!({"address":address,"argv":["/fixture/opencode","--session",session],"cwd":"/fixture/project",
        "created_ms":1234,"state":"live","screen":"exact","cols":200,"rows":120,
        "labels":{"codr.opencode-session":session},"status":{"state":"idle"},"reported":true})
}

async fn snapshot(rows: Vec<Value>, id: &str) -> anyhow::Result<Vec<herdr::Agent>> {
    let (_root, socket, task) = fake(vec![
        ("session.snapshot", absent()),
        ("rpc.discover", schema()),
        ("hello", hello(id)),
        (
            "terminal.list",
            json!({"terminals":rows,"hidden_finished":0,"machines":[]}),
        ),
    ])
    .await;
    let result = herdr::snapshot(&socket).await;
    task.await.unwrap();
    result
}

#[tokio::test]
async fn core_discovery_requires_explicit_local_argv_session_proof_not_name_or_status_guesses() {
    let mut unrelated = terminal("t2", "ses_other");
    unrelated["labels"] = json!({});
    unrelated["name"] = json!("opencode");
    let remote = terminal("remote/t1", "ses_remote");
    let mut unknown = terminal("t3", "ses_unknown");
    unknown["reported"] = json!(false);
    let rows = snapshot(
        vec![terminal("t1", "ses_one"), unrelated, remote, unknown],
        "core-1",
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].settled());
    assert!(!rows[1].settled());
    let mut spoof = terminal("t1", "ses_one");
    spoof["argv"][2] = json!("ses_other");
    assert!(snapshot(vec![spoof], "core-1").await.is_err());
}

#[tokio::test]
async fn core_identity_includes_core_address_and_session_and_rejects_collisions() {
    let first = snapshot(vec![terminal("t1", "ses_one")], "core-1")
        .await
        .unwrap()
        .remove(0);
    let other_core = snapshot(vec![terminal("t1", "ses_one")], "core-2")
        .await
        .unwrap()
        .remove(0);
    let other_address = snapshot(vec![terminal("t2", "ses_one")], "core-1")
        .await
        .unwrap()
        .remove(0);
    let other_session = snapshot(vec![terminal("t1", "ses_other")], "core-1")
        .await
        .unwrap()
        .remove(0);
    assert_ne!(first.key(), other_core.key());
    assert_ne!(first.key(), other_address.key());
    assert_ne!(first.key(), other_session.key());
    assert!(
        snapshot(
            vec![terminal("t1", "ses_one"), terminal("t2", "ses_one")],
            "core-1"
        )
        .await
        .is_err()
    );
    assert!(
        snapshot(
            vec![terminal("t1", "ses_one"), terminal("t1", "ses_other")],
            "core-1"
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn malformed_and_uncertain_records_cannot_become_ready_and_never_launch() {
    for field in [
        "address",
        "argv",
        "cwd",
        "created_ms",
        "state",
        "labels",
        "screen",
        "cols",
        "rows",
    ] {
        let mut row = terminal("t1", "ses_one");
        row.as_object_mut().unwrap().remove(field);
        assert!(
            snapshot(vec![row], "core-1").await.is_err(),
            "missing {field}"
        );
    }
    let mut pending = terminal("t1", "ses_one");
    pending["reason"] = json!("spawn_pending");
    assert!(snapshot(vec![pending], "core-1").await.unwrap().is_empty());
    let mut finished = terminal("t1", "ses_one");
    finished["state"] = json!("finished");
    assert!(snapshot(vec![finished], "core-1").await.unwrap().is_empty());
}

#[tokio::test]
async fn core_terminal_input_and_permission_fallbacks_fail_without_socket_io() {
    let agent = snapshot(vec![terminal("t1", "ses_one")], "core-1")
        .await
        .unwrap()
        .remove(0);
    let socket = Socket {
        path: "/nonexistent/never-connect".into(),
    };
    assert!(
        herdr::prompt(&socket, &agent, "secret")
            .await
            .unwrap_err()
            .to_string()
            .contains("native OpenCode")
    );
    assert!(
        herdr::choose(&socket, &agent, 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
}

#[tokio::test]
async fn legacy_snapshot_keys_are_unchanged() {
    let record = json!({"pane_id":"w1:p1","terminal_id":"legacy-term","agent":"opencode","agent_status":"idle",
        "agent_session":{"source":"herdr:opencode","agent":"opencode","kind":"id","value":"ses_existing"}});
    let (_root, socket, task) = fake(vec![(
        "session.snapshot",
        json!({"snapshot":{"agents":[record]}}),
    )])
    .await;
    let agents = herdr::snapshot(&socket).await.unwrap();
    assert_eq!(
        agents[0].key(),
        "[\"herdr:opencode\",\"opencode\",\"id\",\"ses_existing\"]"
    );
    task.await.unwrap();
}

#[tokio::test]
async fn core_wire_rejects_unmarked_and_ambiguous_replies() {
    for envelope in [
        json!({"result":{}}),
        json!({"jsonrpc":"1.0","result":{}}),
        json!({"jsonrpc":"2.0"}),
        json!({"jsonrpc":"2.0","result":{},"error":{"code":-32000}}),
    ] {
        let (_root, socket, task) =
            fake(vec![("terminal.get", json!({"_envelope":envelope}))]).await;
        assert!(
            socket
                .core("terminal.get", json!({"terminal":"t1"}))
                .await
                .is_err()
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn ambiguous_discovery_error_never_permits_legacy_preflight_fallback() {
    for code in [-32601, -32600] {
        // Even a null result is a second outcome, not an absent result. A valid
        // legacy snapshot must not rescue this malformed capability evidence.
        for result in [schema(), Value::Null] {
            let (_root, socket, task) = fake(vec![
                (
                    "rpc.discover",
                    json!({"_envelope":{"jsonrpc":"2.0","result":result,
                        "error":{"code":code,"message":"rejected"}}}),
                ),
                ("session.snapshot", json!({"snapshot":{"agents":[]}})),
            ])
            .await;
            let outcome = herdr::preflight(&socket).await;
            task.abort();
            assert!(
                outcome.is_err(),
                "ambiguous error {code} selected a backend"
            );
        }
    }
}

#[tokio::test]
async fn malformed_protocol_errors_are_not_legacy_capability_evidence() {
    for error in [
        json!({"jsonrpc":"1.0","error":{"code":-32601,"message":"missing"}}),
        json!({"jsonrpc":"2.0","error":{"code":-32600}}),
    ] {
        let (_root, socket, task) = fake(vec![
            ("rpc.discover", json!({"_envelope":error})),
            ("session.snapshot", json!({"snapshot":{"agents":[]}})),
        ])
        .await;
        let result = herdr::preflight(&socket).await;
        task.abort();
        assert!(result.is_err(), "malformed error selected the old backend");
    }
}

#[tokio::test]
async fn explicit_missing_discovery_can_prove_legacy_without_creation() {
    for error in [
        absent(),
        // The old TUI does not mark its responses as JSON-RPC 2.0.
        json!({"_envelope":{"error":{"code":-32600,"message":"Invalid request"}}}),
    ] {
        let (_root, socket, task) = fake(vec![
            ("rpc.discover", error),
            ("session.snapshot", json!({"snapshot":{"agents":[]}})),
        ])
        .await;
        assert_eq!(
            herdr::preflight(&socket).await.unwrap(),
            herdr::Backend::Tui
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn core_detection_handles_strict_jsonrpc_rejection_of_the_old_envelope() {
    let (_root, socket, task) = fake(vec![
        (
            "session.snapshot",
            json!({"_error":{"code":-32600,"message":"invalid request"}}),
        ),
        ("rpc.discover", schema()),
        ("hello", hello("core-1")),
        (
            "terminal.list",
            json!({"terminals":[],"hidden_finished":0,"machines":[]}),
        ),
    ])
    .await;
    assert!(herdr::snapshot(&socket).await.unwrap().is_empty());
    task.await.unwrap();
}

#[tokio::test]
async fn core_get_rejects_identity_changes_between_snapshot_and_dispatch() {
    let agent = snapshot(vec![terminal("t1", "ses_one")], "core-1")
        .await
        .unwrap()
        .remove(0);
    for (id, record) in [
        ("core-2", terminal("t1", "ses_one")),
        ("core-1", terminal("t2", "ses_one")),
        ("core-1", terminal("t1", "ses_other")),
    ] {
        let (_root, socket, task) = fake(vec![
            ("rpc.discover", schema()),
            ("hello", hello(id)),
            ("terminal.get", record),
        ])
        .await;
        assert!(herdr::get(&socket, &agent).await.is_err());
        task.await.unwrap();
    }
}

fn hello(id: &str) -> Value {
    let methods = schema()["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].clone())
        .collect::<Vec<_>>();
    json!({"core":{"id":id,"name":"not-an-identity"},"generation":1,"version":"0.1.22","framed":false,"methods":methods,"features":[]})
}

#[tokio::test]
async fn malformed_discovery_document_is_not_a_capability_proof() {
    for field in ["openrpc", "info"] {
        let mut document = schema();
        document.as_object_mut().unwrap().remove(field);
        let (_root, socket, task) =
            fake(vec![("rpc.discover", document), ("hello", hello("core-1"))]).await;
        let result = herdr::preflight(&socket).await;
        task.abort();
        assert!(result.is_err(), "Core accepted discovery without {field}");
    }
}

#[tokio::test]
async fn core_preflight_is_read_only_and_checks_identity_and_capabilities() {
    let (_root, socket, task) =
        fake(vec![("rpc.discover", schema()), ("hello", hello("core-1"))]).await;
    assert_eq!(
        herdr::preflight(&socket).await.unwrap(),
        herdr::Backend::Core {
            id: "core-1".into()
        }
    );
    task.await.unwrap();
    let mut mixed = schema();
    mixed["methods"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"session.snapshot"}));
    let (_root, socket, task) = fake(vec![("rpc.discover", mixed)]).await;
    assert!(herdr::preflight(&socket).await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn malformed_core_hello_never_falls_back_to_legacy_or_creates_anything() {
    for invalid in [
        json!({}),
        {
            let mut h = hello("core-1");
            h["generation"] = json!(2);
            h
        },
        {
            let mut h = hello("core-1");
            h["methods"] = json!([]);
            h
        },
    ] {
        let (_root, socket, task) =
            fake(vec![("rpc.discover", schema()), ("hello", invalid)]).await;
        assert!(herdr::preflight(&socket).await.is_err());
        task.await.unwrap();
    }
}
