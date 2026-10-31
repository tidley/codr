use herdr_white_noise_bridge::{BridgeConfig, output};
use serde_json::json;
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};

#[tokio::test]
async fn service_reads_reuse_history_and_never_launch_the_cli() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let paths = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..7 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 8192];
            let size = socket.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..size]);
            assert!(request.to_lowercase().contains("authorization: basic "));
            let path = request.split_whitespace().nth(1).unwrap().to_string();
            seen.lock().unwrap().push(path.clone());
            let body = if path.contains("/log?") {
                log(5)
            } else if path.contains("/message?") {
                history_page("Hello")
            } else {
                json!({"data":[]}).to_string()
            };
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    });
    let file = root.path().join("service.json");
    std::fs::write(
        &file,
        json!({"url":url,"pid":1,"password":"fixture-only"}).to_string(),
    )
    .unwrap();
    let mut value: toml::Value = toml::from_str(include_str!("../config.example.toml")).unwrap();
    value.as_table_mut().unwrap().insert(
        "opencode_service_file".into(),
        file.to_string_lossy().to_string().into(),
    );
    value["opencode_bin"] = "/nonexistent/cli-must-not-run".into();
    let config: BridgeConfig = value.try_into().unwrap();
    let reader = output::service::Reader::default();
    for _ in 0..2 {
        let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(snapshot.history.recent_messages[0].text, "Hello");
    }
    server.join().unwrap();
    let paths = paths.lock().unwrap();
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.contains("/message?"))
            .count(),
        1
    );
    assert!(
        paths
            .iter()
            .filter(|path| path.contains("/log?"))
            .all(|path| path.ends_with("after=9007199254740991&follow=false"))
    );
    assert!(
        paths
            .iter()
            .filter(|path| path.contains("/log?"))
            .all(|path| path.starts_with("/api/experimental/session/ses_fixture/"))
    );
    assert!(
        paths
            .iter()
            .filter(|path| path.contains("/message?"))
            .all(|path| path.starts_with("/api/session/ses_fixture/message?"))
    );
    assert!(
        paths
            .iter()
            .filter(|path| path.ends_with("/form") || path.ends_with("/permission"))
            .all(|path| path.starts_with("/api/session/ses_fixture/"))
    );
}

#[tokio::test]
async fn missing_service_is_retryable_without_cli_fallback() {
    let mut value: toml::Value = toml::from_str(include_str!("../config.example.toml")).unwrap();
    value.as_table_mut().unwrap().insert(
        "opencode_service_file".into(),
        "/nonexistent/service.json".into(),
    );
    value["opencode_bin"] = "/nonexistent/cli-must-not-run".into();
    let config: BridgeConfig = value.try_into().unwrap();
    let reader = output::service::Reader::default();
    for _ in 0..2 {
        assert!(reader.read(&config, "ses_fixture").await.is_err());
    }
    let session = herdr_white_noise_bridge::herdr::Session {
        source: "test".into(),
        agent: "opencode".into(),
        kind: "id".into(),
        value: "ses_fixture".into(),
    };
    assert!(
        output::read_history(&config, &session)
            .await
            .unwrap_err()
            .to_string()
            .contains("registration unavailable")
    );
}

fn config(file: &std::path::Path) -> BridgeConfig {
    let mut value: toml::Value = toml::from_str(include_str!("../config.example.toml")).unwrap();
    value.as_table_mut().unwrap().insert(
        "opencode_service_file".into(),
        file.to_string_lossy().to_string().into(),
    );
    value["opencode_bin"] = "/nonexistent/cli-must-not-run".into();
    value.try_into().unwrap()
}

fn fixture(responses: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 1024];
                let size = socket.read(&mut chunk).unwrap();
                assert!(size > 0);
                bytes.extend_from_slice(&chunk[..size]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            paths.push(
                String::from_utf8_lossy(&bytes)
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_string(),
            );
            write!(
                socket,
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
        paths
    });
    (url, server)
}

fn log(seq: u64) -> String {
    format!(
        "event: message\r\ndata: {}\r\n\r\n",
        json!({"type":"log.synced","aggregateID":"ses_fixture","seq":seq})
    )
}

fn history_page(text: &str) -> String {
    json!({"data":[{"type":"user","id":"msg_1","text":text,"time":{"created":1000}}],"cursor":{"next":null,"previous":null}})
        .to_string()
}

fn page(messages: Vec<serde_json::Value>, next: Option<&str>) -> String {
    json!({"data":messages,"cursor":{"next":next,"previous":null}}).to_string()
}

fn user(id: &str, text: &str) -> serde_json::Value {
    json!({"type":"user","id":id,"text":text,"time":{"created":1000}})
}

fn assistant(id: &str, text: &str, completed: bool) -> serde_json::Value {
    json!({"type":"assistant","id":id,"finish":completed.then_some("stop"),
        "time":{"created":2000,"completed":completed.then_some(3000)},"content":[{"type":"text","text":text}]})
}

fn changes(seq: u64, events: Vec<(u64, &str, serde_json::Value)>) -> String {
    let mut result = String::new();
    for (seq, kind, data) in events {
        result += &format!(
            "data: {}\n\n",
            json!({"type":kind,"durable":{"aggregateID":"ses_fixture","seq":seq,"version":1},"data":data})
        );
    }
    result + &log(seq)
}

#[tokio::test]
async fn incremental_history_reads_only_the_new_suffix_and_retains_the_prefix() {
    let empty = || (200, json!({"data":[]}).to_string());
    let old: Vec<_> = (0..250)
        .map(|i| user(&format!("msg_{i}"), "Old input"))
        .collect();
    let mut responses = vec![(200, log(5))];
    for (i, chunk) in old.chunks(100).enumerate() {
        responses.push((
            200,
            page(
                chunk.to_vec(),
                (i < 2).then_some(if i == 0 { "second" } else { "third" }),
            ),
        ));
    }
    responses.extend([
        empty(),
        empty(),
        (200, log(6)),
        (
            200,
            changes(
                6,
                vec![(
                    6,
                    "session.inbox.delivered",
                    json!({"sessionID":"ses_fixture","inboxID":"msg_250"}),
                )],
            ),
        ),
        (
            200,
            page(
                vec![user("msg_250", "New input"), old[249].clone()],
                Some("must-not-fetch"),
            ),
        ),
        (200, log(6)),
        empty(),
        empty(),
        (200, log(6)),
        empty(),
        empty(),
    ]);
    let (url, server) = fixture(responses);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    assert_eq!(
        reader
            .read(&config, "ses_fixture")
            .await
            .unwrap()
            .history
            .user_inputs
            .len(),
        250
    );
    for _ in 0..2 {
        let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(snapshot.history.user_inputs.len(), 251);
        assert_eq!(snapshot.history.user_inputs[0].id, "opencode:msg_0");
        assert_eq!(
            snapshot.history.latest_turn.as_deref(),
            Some("opencode:msg_250")
        );
    }
    let paths = server.join().unwrap();
    assert_eq!(paths.iter().filter(|p| p.contains("/message?")).count(), 4);
    assert_eq!(paths.iter().filter(|p| p.contains("order=desc")).count(), 1);
    assert!(paths.iter().any(|p| p.contains("after=5&follow=false")));
    assert!(!paths.iter().any(|p| p.contains("must-not-fetch")));
}

#[tokio::test]
async fn incremental_history_refreshes_an_older_changed_assistant_not_just_the_last_known_id() {
    let first = user("msg_1", "First");
    let later = user("msg_3", "Later");
    let empty = || (200, json!({"data":[]}).to_string());
    let (url, server) = fixture(vec![
        (200, log(5)),
        (
            200,
            page(
                vec![
                    first.clone(),
                    assistant("msg_2", "Original reply", true),
                    later.clone(),
                ],
                None,
            ),
        ),
        empty(),
        empty(),
        (200, log(6)),
        (
            200,
            changes(
                6,
                vec![(
                    6,
                    "session.step.ended",
                    json!({"sessionID":"ses_fixture","assistantMessageID":"msg_2"}),
                )],
            ),
        ),
        (
            200,
            page(
                vec![later, assistant("msg_2", "Completed reply", true)],
                Some("older"),
            ),
        ),
        (200, page(vec![first], None)),
        (200, log(6)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    assert_eq!(
        reader
            .read(&config, "ses_fixture")
            .await
            .unwrap()
            .history
            .finals[0]
            .text,
        "Original reply"
    );
    let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
    assert_eq!(snapshot.history.finals[0].text, "Completed reply");
    assert_eq!(
        snapshot.history.finals[0].turn.as_deref(),
        Some("opencode:msg_1")
    );
    assert_eq!(
        snapshot.history.latest_turn.as_deref(),
        Some("opencode:msg_3")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn paged_history_keeps_turns_and_finals_without_fetching_a_whole_export() {
    let empty = || (200, json!({"data":[]}).to_string());
    let cursor = "opaque/+= &?";
    let (url, server) = fixture(vec![
        (200, log(5)),
        (200, json!({"data":[
            {"type":"user","id":"msg_1","text":"First request","time":{"created":1000}},
            {"type":"assistant","id":"tool_1","content":[{"type":"tool","name":"read","state":{"output":"unused tool output"}}]}
        ],"cursor":{"next":cursor,"previous":null}}).to_string()),
        (200, json!({"data":[
            {"type":"assistant","id":"msg_2","finish":"stop","time":{"created":2000,"completed":3000},"content":[{"type":"text","text":"First reply"}]},
            {"type":"user","id":"msg_3","text":"Next request","time":{"created":4000}}
        ],"cursor":{"next":null,"previous":"earlier"}}).to_string()),
        empty(),empty(),
        (200, log(5)),empty(),empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    for _ in 0..2 {
        let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(snapshot.history.user_inputs.len(), 2);
        assert_eq!(snapshot.history.finals.len(), 1);
        assert_eq!(
            snapshot.history.finals[0].turn.as_deref(),
            Some("opencode:msg_1")
        );
        assert_eq!(
            snapshot.history.latest_turn.as_deref(),
            Some("opencode:msg_3")
        );
        assert_eq!(snapshot.history.last_messages.len(), 3);
    }
    let paths = server.join().unwrap();
    assert!(!paths.iter().any(|path| path.ends_with("/export")));
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.contains("/message?"))
            .count(),
        2
    );
    assert!(paths[1].contains("order=asc"));
    assert!(paths[2].contains("cursor=opaque%2F%2B%3D+%26%3F"));
    assert!(!paths[2].contains("order="));
}

#[tokio::test]
async fn incremental_untrusted_deltas_fall_back_to_complete_history() {
    let valid = changes(
        6,
        vec![(
            6,
            "session.inbox.delivered",
            json!({"sessionID":"ses_fixture","inboxID":"msg_new"}),
        )],
    );
    let cases = vec![
        (200, log(6)), // Public log filtered out the actual event.
        (
            200,
            changes(
                6,
                vec![(
                    6,
                    "session.revert.committed",
                    json!({"sessionID":"ses_fixture"}),
                )],
            ),
        ),
        (200, valid.replace("\"version\":1", "\"version\":2")),
        (
            200,
            valid.replace(
                "\"sessionID\":\"ses_fixture\"",
                "\"sessionID\":\"ses_other\"",
            ),
        ),
        (
            200,
            valid.replace("\"seq\":6,\"version\"", "\"seq\":7,\"version\""),
        ),
        (200, valid.replace("\"inboxID\":\"msg_new\",", "")),
        (200, "data: {\"type\":\"log.synced\"".into()),
        (200, valid + &" ".repeat(1024 * 1024)),
        (503, "unavailable".into()),
    ];
    for delta in cases {
        let empty = || (200, json!({"data":[]}).to_string());
        let (url, server) = fixture(vec![
            (200, log(5)),
            (200, page(vec![user("old", "Before")], None)),
            empty(),
            empty(),
            (200, log(6)),
            delta,
            (200, page(vec![user("replacement", "Rewritten")], None)),
            empty(),
            empty(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("service.json");
        std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
        let config = config(&file);
        let reader = output::service::Reader::default();
        reader.read(&config, "ses_fixture").await.unwrap();
        let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(snapshot.history.user_inputs.len(), 1);
        assert_eq!(snapshot.history.user_inputs[0].id, "opencode:replacement");
        let paths = server.join().unwrap();
        assert_eq!(paths.iter().filter(|p| p.contains("order=asc")).count(), 2);
        assert!(!paths.iter().any(|p| p.contains("order=desc")));
    }
}

#[tokio::test]
async fn incremental_refreshes_unfinished_messages_even_when_the_delta_only_adds_a_user() {
    let empty = || (200, json!({"data":[]}).to_string());
    let first = user("first", "First");
    let last = user("last", "Last");
    let (url, server) = fixture(vec![
        (200, log(5)),
        (
            200,
            page(
                vec![
                    first.clone(),
                    assistant("open", "Working", false),
                    last.clone(),
                ],
                None,
            ),
        ),
        empty(),
        empty(),
        (200, log(6)),
        (
            200,
            changes(
                6,
                vec![(
                    6,
                    "session.inbox.delivered",
                    json!({"sessionID":"ses_fixture","inboxID":"new"}),
                )],
            ),
        ),
        (
            200,
            page(
                vec![
                    user("new", "New"),
                    last,
                    assistant("open", "Completed", true),
                    first,
                ],
                None,
            ),
        ),
        (200, log(6)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    reader.read(&config, "ses_fixture").await.unwrap();
    let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
    assert_eq!(snapshot.history.finals[0].text, "Completed");
    assert_eq!(
        snapshot.history.finals[0].turn.as_deref(),
        Some("opencode:first")
    );
    assert_eq!(snapshot.history.user_inputs.len(), 3);
    server.join().unwrap();
}

#[tokio::test]
async fn incremental_missing_boundary_replaces_the_entire_cache() {
    let empty = || (200, json!({"data":[]}).to_string());
    let (url, server) = fixture(vec![
        (200, log(5)),
        (
            200,
            page(
                vec![user("first", "First"), user("missing", "Boundary")],
                None,
            ),
        ),
        empty(),
        empty(),
        (200, log(6)),
        (
            200,
            changes(
                6,
                vec![(
                    6,
                    "session.inbox.delivered",
                    json!({"sessionID":"ses_fixture","inboxID":"new"}),
                )],
            ),
        ),
        (200, page(vec![user("new", "New")], None)),
        (200, log(6)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    reader.read(&config, "ses_fixture").await.unwrap();
    let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
    assert_eq!(snapshot.history.user_inputs.len(), 1);
    assert_eq!(
        snapshot.history.latest_turn.as_deref(),
        Some("opencode:new")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn incremental_version_two_tool_events_refresh_tasks_and_the_earliest_changed_message() {
    let empty = || (200, json!({"data":[]}).to_string());
    let task_message = |text: &str| {
        json!({"type":"assistant","id":"msg_4","finish":"stop","time":{"created":4000,"completed":5000},"content":[
            {"type":"tool","name":"read","state":{"output":"discard this"}},
            {"type":"tool","name":"todowrite","state":{"status":"completed","input":{"todos":[{"content":text,"status":"in_progress"}]}}}
        ]})
    };
    let first = user("msg_1", "First");
    let later = user("msg_3", "Later");
    let delta = changes(
        7,
        vec![
            (
                6,
                "session.tool.success",
                json!({"sessionID":"ses_fixture","assistantMessageID":"msg_4"}),
            ),
            (
                7,
                "session.tool.failed",
                json!({"sessionID":"ses_fixture","assistantMessageID":"msg_2"}),
            ),
        ],
    )
    .replace("\"version\":1", "\"version\":2");
    let (url, server) = fixture(vec![
        (200, log(5)),
        (
            200,
            page(
                vec![
                    first.clone(),
                    assistant("msg_2", "Original", true),
                    later.clone(),
                    task_message("Old task"),
                ],
                None,
            ),
        ),
        empty(),
        empty(),
        (200, log(7)),
        (200, delta),
        (
            200,
            page(
                vec![
                    task_message("Updated task"),
                    later,
                    assistant("msg_2", "Edited reply", true),
                ],
                Some("older"),
            ),
        ),
        (200, page(vec![first], None)),
        (200, log(7)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    reader.read(&config, "ses_fixture").await.unwrap();
    let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
    assert_eq!(snapshot.history.user_inputs.len(), 2);
    assert_eq!(snapshot.history.finals[0].text, "Edited reply");
    let tasks = snapshot.tasks.unwrap();
    assert_eq!(tasks.items[0].content, "Updated task");
    assert_eq!(tasks.revision, "opencode:msg_4:1");
    let paths = server.join().unwrap();
    assert_eq!(paths.iter().filter(|p| p.contains("order=asc")).count(), 1);
    assert_eq!(paths.iter().filter(|p| p.contains("order=desc")).count(), 1);
}

#[tokio::test]
async fn incremental_boundary_content_mismatch_rebuilds_the_descending_timeline() {
    let empty = || (200, json!({"data":[]}).to_string());
    let (url, server) = fixture(vec![
        (200, log(5)),
        (
            200,
            page(
                vec![
                    user("first", "Old prefix"),
                    user("boundary", "Old boundary"),
                ],
                None,
            ),
        ),
        empty(),
        empty(),
        (200, log(6)),
        (
            200,
            changes(
                6,
                vec![(
                    6,
                    "session.inbox.delivered",
                    json!({"sessionID":"ses_fixture","inboxID":"new"}),
                )],
            ),
        ),
        (
            200,
            page(
                vec![user("new", "New"), user("boundary", "Changed boundary")],
                Some("older"),
            ),
        ),
        (200, page(vec![user("first", "Changed prefix")], None)),
        (200, log(6)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    reader.read(&config, "ses_fixture").await.unwrap();
    let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
    assert_eq!(snapshot.history.recent_messages[0].text, "Changed prefix");
    assert_eq!(snapshot.history.recent_messages[1].text, "Changed boundary");
    assert_eq!(snapshot.history.user_inputs.len(), 3);
    server.join().unwrap();
}

#[tokio::test]
async fn incremental_control_failures_and_read_races_leave_the_old_cache_intact() {
    for race in [false, true] {
        let empty = || (200, json!({"data":[]}).to_string());
        let delta = || {
            changes(
                6,
                vec![(
                    6,
                    "session.inbox.delivered",
                    json!({"sessionID":"ses_fixture","inboxID":"new"}),
                )],
            )
        };
        let mut responses = vec![
            (200, log(5)),
            (200, page(vec![user("old", "Old")], None)),
            empty(),
            empty(),
            (200, log(6)),
            (200, delta()),
            (
                200,
                page(
                    vec![user("discarded", "Discarded"), user("old", "Old")],
                    Some("unused"),
                ),
            ),
            (200, log(if race { 7 } else { 6 })),
        ];
        if !race {
            responses.push((503, "unavailable".into()));
        }
        responses.extend([
            (200, log(6)),
            (200, delta()),
            (
                200,
                page(
                    vec![user("new", "Confirmed"), user("old", "Old")],
                    Some("unused"),
                ),
            ),
            (200, log(6)),
            empty(),
            empty(),
        ]);
        let (url, server) = fixture(responses);
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("service.json");
        std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
        let config = config(&file);
        let reader = output::service::Reader::default();
        reader.read(&config, "ses_fixture").await.unwrap();
        assert!(reader.read(&config, "ses_fixture").await.is_err());
        let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(snapshot.history.user_inputs.len(), 2);
        assert_eq!(snapshot.history.user_inputs[1].id, "opencode:new");
        let paths = server.join().unwrap();
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.contains("after=5&follow=false"))
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn missing_or_regressed_watermarks_do_not_reuse_old_history() {
    let missing = "data: {\"type\":\"log.synced\",\"aggregateID\":\"ses_fixture\"}\n\n".to_string();
    for (first, next) in [(missing.clone(), missing), (log(5), log(4))] {
        let empty = || (200, json!({"data":[]}).to_string());
        let (url, server) = fixture(vec![
            (200, first),
            (200, history_page("Before")),
            empty(),
            empty(),
            (200, next),
            (200, history_page("After")),
            empty(),
            empty(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("service.json");
        std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
        let config = config(&file);
        let reader = output::service::Reader::default();
        reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(
            reader
                .read(&config, "ses_fixture")
                .await
                .unwrap()
                .history
                .recent_messages[0]
                .text,
            "After"
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn failed_or_cyclic_page_does_not_commit_a_partial_history() {
    let page = |text: &str, cursor: Option<&str>| {
        json!({"data":[
        {"type":"user","id":text,"text":text,"time":{"created":1000}}
    ],"cursor":{"next":cursor,"previous":null}})
        .to_string()
    };
    let empty = || (200, json!({"data":[]}).to_string());
    let (url, server) = fixture(vec![
        (200, log(5)),
        (200, page("known", None)),
        empty(),
        empty(),
        (200, log(6)),
        (200, log(6)), // A missing delta selects the full read.
        (200, page("partial", Some("next"))),
        (503, "unavailable".into()),
        (200, log(6)),
        (200, log(6)),
        (200, page("partial", Some("repeat"))),
        (200, page("other", Some("repeat"))),
        (200, log(6)),
        (200, log(6)),
        (200, page("recovered", None)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    assert_eq!(
        reader
            .read(&config, "ses_fixture")
            .await
            .unwrap()
            .history
            .recent_messages[0]
            .text,
        "known"
    );
    assert!(reader.read(&config, "ses_fixture").await.is_err());
    assert!(reader.read(&config, "ses_fixture").await.is_err());
    assert_eq!(
        reader
            .read(&config, "ses_fixture")
            .await
            .unwrap()
            .history
            .recent_messages[0]
            .text,
        "recovered"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn discarded_tool_output_keeps_task_part_positions_across_pages() {
    let (url,server)=fixture(vec![
        (200,log(5)),
        (200,json!({"data":[{"type":"user","id":"msg_1","text":"Work","time":{"created":1000}}],"cursor":{"next":"next"}}).to_string()),
        (200,json!({"data":[{"type":"assistant","id":"msg_2","content":[
            {"type":"tool","name":"read","state":{"output":"unused".repeat(400_000)}},
            {"type":"tool","name":"todowrite","state":{"status":"completed","input":{"todos":[{"content":"Keep this task","status":"in_progress"}]}}}
        ]}],"cursor":{"next":null}}).to_string()),
        (200,json!({"data":[]}).to_string()),(200,json!({"data":[]}).to_string()),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let snapshot = output::service::Reader::default()
        .read(&config(&file), "ses_fixture")
        .await
        .unwrap();
    let tasks = snapshot.tasks.unwrap();
    assert_eq!(tasks.revision, "opencode:msg_2:1");
    assert_eq!(tasks.items[0].content, "Keep this task");
    assert_eq!(snapshot.history.last_messages.len(), 1);
    server.join().unwrap();
}

#[tokio::test]
async fn invalid_pagination_fails_closed_without_reading_controls() {
    let message =
        |id: &str| json!({"type":"user","id":id,"text":"fixture","time":{"created":1000}});
    let page = |data: Vec<serde_json::Value>, next: Option<String>| json!({"data":data,"cursor":{"next":next}});
    let cases = [
        vec![
            page(vec![message("same")], Some("next".into())),
            page(vec![message("same")], None),
        ],
        vec![page(vec![], Some("empty".into()))],
        vec![page(
            (0..101).map(|i| message(&format!("msg_{i}"))).collect(),
            None,
        )],
        vec![page(vec![message("one")], Some("x".repeat(16 * 1024 + 1)))],
    ];
    for pages in cases {
        let mut responses = vec![(200, log(5))];
        responses.extend(pages.into_iter().map(|page| (200, page.to_string())));
        let (url, server) = fixture(responses);
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("service.json");
        std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
        assert!(
            output::service::Reader::default()
                .read(&config(&file), "ses_fixture")
                .await
                .is_err()
        );
        let paths = server.join().unwrap();
        assert!(
            paths
                .iter()
                .all(|p| p.contains("/log?") || p.contains("/message?"))
        );
    }
}

#[tokio::test]
async fn oversized_history_page_is_rejected_from_its_headers_without_buffering_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for first in [true, false] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 8192];
            assert!(socket.read(&mut request).unwrap() > 0);
            if first {
                let body = log(5);
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            } else {
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    128 * 1024 * 1024 + 1
                )
                .unwrap();
            }
        }
    });
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let error = output::service::Reader::default()
        .read(&config(&file), "ses_fixture")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exceeded size limit"));
    server.join().unwrap();
}

#[tokio::test]
async fn changed_history_rebuilds_and_failed_prompts_do_not_advance_cursor() {
    let ok = |body: String| (200, body);
    let empty = || ok(json!({"data":[]}).to_string());
    let (url, server) = fixture(vec![
        ok(log(5)),
        ok(history_page("Before")),
        empty(),
        empty(),
        ok(log(6)),
        ok(log(6)),
        ok(history_page("After")),
        (503, "unavailable".into()),
        ok(log(6)),
        ok(log(6)),
        ok(history_page("After")),
        empty(),
        empty(),
        ok(log(6)),
        empty(),
        empty(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    assert_eq!(
        reader
            .read(&config, "ses_fixture")
            .await
            .unwrap()
            .history
            .recent_messages[0]
            .text,
        "Before"
    );
    assert!(reader.read(&config, "ses_fixture").await.is_err());
    for _ in 0..2 {
        assert_eq!(
            reader
                .read(&config, "ses_fixture")
                .await
                .unwrap()
                .history
                .recent_messages[0]
                .text,
            "After"
        );
    }
    let paths = server.join().unwrap();
    for index in [0, 4, 8, 13] {
        assert!(paths[index].ends_with("after=9007199254740991&follow=false"));
    }
}

#[tokio::test]
async fn a_new_registered_owner_rebuilds_history_at_the_same_watermark() {
    let empty = json!({"data":[]}).to_string();
    let (url, server) = fixture(vec![
        (200, log(5)),
        (200, history_page("Old owner")),
        (200, empty.clone()),
        (200, empty.clone()),
        (200, log(5)),
        (200, history_page("New owner")),
        (200, empty.clone()),
        (200, empty),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    let config = config(&file);
    let reader = output::service::Reader::default();
    for (pid, text) in [(1, "Old owner"), (2, "New owner")] {
        std::fs::write(&file, json!({"url":url,"pid":pid}).to_string()).unwrap();
        assert_eq!(
            reader
                .read(&config, "ses_fixture")
                .await
                .unwrap()
                .history
                .recent_messages[0]
                .text,
            text
        );
    }
    assert_eq!(
        server
            .join()
            .unwrap()
            .iter()
            .filter(|path| path.contains("/message?"))
            .count(),
        2
    );
}

#[tokio::test]
async fn invalid_logs_and_http_errors_never_fall_back_to_cli() {
    let (url, server) = fixture(vec![
        (503, "unavailable".into()),
        (200, "".into()),
        (
            200,
            "data: {\"type\":\"log.synced\",\"aggregateID\":\"ses_other\",\"seq\":5}\r\n\r\n".into(),
        ),
        (
            200,
            "data: {\"type\":\"log.synced\",\"aggregateID\":\"ses_fixture\",\"seq\":\"bad\"}\r\n\r\n".into(),
        ),
        (200, log(5) + "data: {}\n\n"),
    ]);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    for _ in 0..5 {
        assert!(reader.read(&config, "ses_fixture").await.is_err());
    }
    assert!(
        server
            .join()
            .unwrap()
            .iter()
            .all(|path| path.contains("/log?"))
    );
}

#[tokio::test]
async fn registration_rejects_nonlocal_and_credential_bearing_urls() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    let config = config(&file);
    let reader = output::service::Reader::default();
    for url in [
        "http://192.0.2.1:1234",
        "https://127.0.0.1:1234",
        "http://user:secret@127.0.0.1:1234",
        "http://127.0.0.1:1234/path",
    ] {
        std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
        assert_eq!(
            reader
                .read(&config, "ses_fixture")
                .await
                .unwrap_err()
                .to_string(),
            "OpenCode service must use a loopback HTTP address"
        );
    }
}

#[tokio::test]
async fn representative_history_is_read_once_without_replaying_its_event_log() {
    let messages: Vec<_> = (0..5000)
        .map(|i| {
            json!({
                "type":"user", "id":format!("msg_{i}"), "text":"x".repeat(256),
                "time":{"created":1000+i},
            })
        })
        .collect();
    let empty = json!({"data":[]}).to_string();
    let mut responses = vec![(200, ": heartbeat\r\n\r\n".to_string() + &log(50_000))];
    for (index, page) in messages.chunks(100).enumerate() {
        responses.push((200,json!({"data":page,"cursor":{"next":(index<49).then(|| format!("page-{index}")),"previous":null}}).to_string()));
    }
    responses.extend([
        (200, empty.clone()),
        (200, empty.clone()),
        (200, log(50_000)),
        (200, empty.clone()),
        (200, empty),
    ]);
    let (url, server) = fixture(responses);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, json!({"url":url,"pid":1}).to_string()).unwrap();
    let config = config(&file);
    let reader = output::service::Reader::default();
    for _ in 0..2 {
        let snapshot = reader.read(&config, "ses_fixture").await.unwrap();
        assert_eq!(snapshot.history.recent_messages.len(), 3);
        assert_eq!(snapshot.history.last_messages.len(), 100);
        assert_eq!(snapshot.history.last_messages[99].id, "msg_4999");
        assert_eq!(snapshot.history.recent_messages[2].id, "msg_4999");
        assert_eq!(snapshot.history.user_inputs.len(), 5000);
        assert_eq!(snapshot.history.turns.len(), 5000);
        assert_eq!(snapshot.history.user_inputs[0].id, "opencode:msg_0");
        assert_eq!(snapshot.history.user_inputs[4999].id, "opencode:msg_4999");
        assert_eq!(
            snapshot.history.latest_turn.as_deref(),
            Some("opencode:msg_4999")
        );
        assert_eq!(snapshot.history.activity_at, Some(5));
    }
    let paths = server.join().unwrap();
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.contains("/message?"))
            .count(),
        50
    );
    assert!(
        paths
            .iter()
            .filter(|path| path.contains("/log?"))
            .all(|path| path.ends_with("after=9007199254740991&follow=false"))
    );
}
