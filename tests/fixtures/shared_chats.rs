//! All peers are isolated sockets; no account storage, live sessions or models.
use super::*;
use std::collections::BTreeMap;
use tokio::io::AsyncReadExt;

#[path = "strict_rooms.rs"]
mod strict_rooms;

struct Fixture {
    root: tempfile::TempDir,
    config: BridgeConfig,
    agents: Arc<Mutex<Vec<Agent>>>,
    posts: Arc<Mutex<Vec<(String, Value)>>>,
    histories: Arc<Mutex<BTreeMap<String, Vec<Value>>>>,
    calls: Arc<Mutex<Vec<Value>>>,
    rows: Arc<Mutex<Vec<Value>>>,
    info: Arc<Mutex<Value>>,
    lose: Arc<Mutex<Vec<String>>>,
    forms: Arc<Mutex<BTreeMap<String, Value>>>,
    permissions: Arc<Mutex<BTreeMap<String, Value>>>,
    failed_reads: Arc<Mutex<Vec<String>>>,
    jobs: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.abort();
        }
    }
}

fn second_agent() -> Agent {
    let mut other = agent();
    other.pane_id = "w1:p2".into();
    other.terminal_id = "term2".into();
    other.agent_session.as_mut().unwrap().value = "ses_2".into();
    other
}

fn final_message(id: &str, text: &str) -> Value {
    json!({"type":"assistant","id":id,"finish":"stop","time":{"created":2000,"completed":3000},"content":[{"type":"text","text":text}]})
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.edit_responses = true;
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let file = root.path().join("service.json");
        std::fs::write(
            &file,
            json!({"url":format!("http://{}",http.local_addr().unwrap()),"pid":1}).to_string(),
        )
        .unwrap();
        config.opencode_service_file = Some(file);
        let posts = Arc::new(Mutex::new(Vec::new()));
        let histories = Arc::new(Mutex::new(BTreeMap::<String, Vec<Value>>::from([
            (
                "ses_1".into(),
                vec![final_message("old-one", "Old builder output")],
            ),
            (
                "ses_2".into(),
                vec![final_message("old-two", "Old reviewer output")],
            ),
        ])));
        let lose = Arc::new(Mutex::new(Vec::new()));
        let recorded = posts.clone();
        let history = histories.clone();
        let lost = lose.clone();
        let forms = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
        let dialogs = forms.clone();
        let permissions = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
        let permission_cards = permissions.clone();
        let failed_reads = Arc::new(Mutex::new(Vec::new()));
        let unavailable = failed_reads.clone();
        let state_path = config.state_path.clone();
        let native = tokio::spawn(async move {
            loop {
                let (mut socket, _) = http.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    bytes.push(socket.read_u8().await.unwrap());
                    if bytes.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let headers = String::from_utf8(bytes).unwrap();
                let line = headers.lines().next().unwrap();
                let path = line.split_whitespace().nth(1).unwrap();
                let session = path
                    .split('/')
                    .find(|part| part.starts_with("ses_"))
                    .unwrap()
                    .to_owned();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                if line.starts_with("GET ") && unavailable.lock().unwrap().contains(&session) {
                    socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                    continue;
                }
                let response = if line.starts_with("POST ") {
                    assert!(
                        path.ends_with("/prompt"),
                        "No permission or tool API mutation is allowed"
                    );
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let state = BridgeState::load(&state_path).unwrap();
                    let mapping = state
                        .mappings
                        .values()
                        .find(|m| {
                            m.agent_record
                                .as_ref()
                                .is_some_and(|a| a.agent_session.as_ref().unwrap().value == session)
                        })
                        .unwrap();
                    assert!(
                        mapping
                            .submissions
                            .iter()
                            .any(|s| s.native_message_id.as_deref() == body["id"].as_str())
                    );
                    recorded
                        .lock()
                        .unwrap()
                        .push((session.clone(), body.clone()));
                    if lost.lock().unwrap().contains(&session) {
                        continue;
                    }
                    json!({"data":{"id":body["id"],"sessionID":session,"type":"user","payload":{"text":body["text"]}}}).to_string()
                } else if path.contains("/message/msg_codr_") {
                    let id = path.rsplit('/').next().unwrap();
                    let posts = recorded.lock().unwrap();
                    let (_, post) = posts
                        .iter()
                        .find(|(ses, post)| ses == &session && post["id"] == id)
                        .unwrap();
                    json!({"data":{"id":id,"type":"user","text":post["text"]}}).to_string()
                } else if path.contains("/log?") {
                    let seq = history.lock().unwrap()[&session].len();
                    format!(
                        "data: {}\n\n",
                        json!({"type":"log.synced","aggregateID":session,"seq":seq})
                    )
                } else if path.contains("/message?") {
                    json!({"data":history.lock().unwrap()[&session],"cursor":{"next":null}})
                        .to_string()
                } else if path.ends_with("/form") {
                    dialogs
                        .lock()
                        .unwrap()
                        .get(&session)
                        .cloned()
                        .unwrap_or_else(|| json!({"data":[]}))
                        .to_string()
                } else if path.ends_with("/permission") {
                    permission_cards
                        .lock()
                        .unwrap()
                        .get(&session)
                        .cloned()
                        .unwrap_or_else(|| json!({"data":[]}))
                        .to_string()
                } else {
                    json!({"data":[]}).to_string()
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            }
        });
        let calls = Arc::new(Mutex::new(vec![]));
        let seen = calls.clone();
        let agents = Arc::new(Mutex::new(vec![agent(), second_agent()]));
        let observed = agents.clone();
        let listener = UnixListener::bind(&config.herdr_socket).unwrap();
        let herdr = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let request = read_line(&mut reader).await.unwrap();
                seen.lock().unwrap().push(request.clone());
                let core = observed.lock().unwrap().first().is_some_and(herdr::is_core);
                if core {
                    let result = match request["method"].as_str().unwrap() {
                        "session.snapshot" => None,
                        "rpc.discover" => Some(
                            serde_json::from_str(include_str!("herdr-core-0.1.22/schema.json"))
                                .unwrap(),
                        ),
                        "hello" => Some(
                            json!({"core":{"id":"fixture-core","name":"desktop"},"version":"0.1.22","generation":1,"framed":false,
                            "features":[],"methods":["hello","server.status","terminal.run","terminal.list","terminal.get"]}),
                        ),
                        "terminal.list" => Some(
                            json!({"terminals":observed.lock().unwrap().iter().map(core_terminal).collect::<Vec<_>>(),"hidden_finished":0,"machines":[]}),
                        ),
                        "terminal.get" => Some(core_terminal(
                            observed
                                .lock()
                                .unwrap()
                                .iter()
                                .find(|a| request["params"]["terminal"] == a.pane_id)
                                .unwrap(),
                        )),
                        method => panic!("Unexpected Core mutation {method}"),
                    };
                    let response = match result {
                        Some(result) => json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                        None => {
                            json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"method not found"}})
                        }
                    };
                    reader
                        .get_mut()
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .unwrap();
                    continue;
                }
                let response = match request["method"].as_str().unwrap() {
                    "session.snapshot" => {
                        json!({"result":{"snapshot":{"agents":*observed.lock().unwrap(),"tabs":[]}}})
                    }
                    "agent.get" => {
                        let agents = observed.lock().unwrap();
                        let current = agents
                            .iter()
                            .find(|a| request["params"]["target"] == a.pane_id)
                            .unwrap();
                        json!({"result":{"agent":current}})
                    }
                    method => panic!("Unexpected Herdr mutation {method}"),
                };
                reader
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id":request["id"],"result":response["result"]})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let rows = Arc::new(Mutex::new(vec![
            json!({"message_id_hex":"old-group","recorded_at":u64::MAX,"kind":9,"availability":"available","sender":{"account_id_hex":HEX},"text":"@builder old"}),
        ]));
        let info = Arc::new(Mutex::new(attachment_info()));
        let mdk = attachment_peer(
            &config.wn_agent_socket,
            calls.clone(),
            info.clone(),
            rows.clone(),
        )
        .await;
        Self {
            root,
            config,
            agents,
            posts,
            histories,
            calls,
            rows,
            info,
            lose,
            forms,
            permissions,
            failed_reads,
            jobs: vec![native, herdr, mdk],
        }
    }

    async fn attached(&self) -> Bridge {
        let mut bridge = Bridge::open(self.config.clone()).unwrap();
        bridge
            .attach_shared_chat(
                &"bb".repeat(16),
                &[app_npub(0x22), app_npub(0x33)],
                &[
                    ("builder".into(), agent().key()),
                    ("reviewer".into(), second_agent().key()),
                ],
            )
            .await
            .unwrap();
        bridge.reconcile(self.snapshot()).await.unwrap();
        assert!(
            bridge.state.outbox.is_empty(),
            "No group or agent history replay on setup"
        );
        bridge
    }

    // Copy inside a synchronous scope: no fixture lock is held across bridge I/O.
    fn snapshot(&self) -> Vec<Agent> {
        self.agents.lock().unwrap().clone()
    }
}

fn core_terminal(agent: &Agent) -> Value {
    json!({"address":agent.pane_id,"argv":["/fixture/opencode","--session",agent.agent_session.as_ref().unwrap().value],
        "cwd":"/fixture/project","created_ms":1234,"state":"live","screen":"exact","cols":200,"rows":120,
        "labels":{"codr.opencode-session":agent.agent_session.as_ref().unwrap().value},"status":{"state":agent.agent_status},"reported":true})
}

fn message(sender: &str, id: &str, text: &str) -> Value {
    let mut event = inbound(sender, id);
    event["message"]["text"] = json!(text);
    event["message"]["recorded_at"] = json!(now().saturating_add(1));
    event
}

#[tokio::test]
async fn shared_chat_unknown_partial_foreign_bot_self_media_and_controls_fail_closed() {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    for (n, text) in [
        "@build hi",
        "@builder-extra hi",
        "@builder @unknown hi",
        "mail@builder https://x/@builder",
        "hello",
        "@builder /sync on",
        "@builder unmute",
        "@reviewer /new",
    ]
    .iter()
    .enumerate()
    {
        bridge
            .inbound(&message(&"22".repeat(32), &format!("invalid-{n}"), text))
            .await
            .unwrap();
    }
    for kind in [
        "foreign",
        "group",
        "unauthorized",
        "self",
        "bot",
        "agent",
        "system",
        "role",
        "system-kind",
        "sender-role",
        "historical",
        "future",
        "media",
        "truncated",
    ] {
        let mut event = message(&"22".repeat(32), kind, "@builder run");
        match kind {
            "foreign" => event["account_id_hex"] = json!("ee".repeat(32)),
            "group" => event["group_id_hex"] = json!("dd".repeat(16)),
            "unauthorized" => event["message"]["sender"]["account_id_hex"] = json!("44".repeat(32)),
            "self" => event["message"]["sender"]["is_self"] = json!(true),
            "bot" => event["message"]["sender"]["is_bot"] = json!(true),
            "agent" => event["message"]["sender"]["is_agent"] = json!(true),
            "system" => event["message"]["sender"]["is_system"] = json!(true),
            "role" => event["message"]["role"] = json!("assistant"),
            "sender-role" => event["message"]["sender"]["role"] = json!("agent"),
            "system-kind" => event["message"]["kind"] = json!(0),
            "historical" => event["message"]["recorded_at"] = json!(0),
            "future" => event["message"]["recorded_at"] = json!(u64::MAX),
            "media" => event["message"]["media"] = json!([{"media_type":"audio/ogg"}]),
            "truncated" => event["message"]["text_truncated"] = json!(true),
            _ => unreachable!(),
        }
        bridge.inbound(&event).await.unwrap();
    }
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(bridge.state.media_pending.is_empty());
    assert!(
        bridge
            .state
            .outbox
            .iter()
            .all(|item| item.group == "bb".repeat(16))
    );
    // Even a corrupt duplicate alias cannot choose the first mapping.
    bridge
        .state
        .mappings
        .get_mut(&second_agent().key())
        .unwrap()
        .shared_alias = Some("builder".into());
    bridge
        .inbound(&message(&"22".repeat(32), "ambiguous", "@builder run"))
        .await
        .unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        bridge
            .state
            .mappings
            .values()
            .all(|m| m.room_quarantined.is_some())
    );
}

#[tokio::test]
async fn shared_chat_setup_is_atomic_rejects_existing_bindings_and_captures_baselines() {
    let fixture = Fixture::new().await;
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    let bindings = [
        ("builder".into(), agent().key()),
        ("reviewer".into(), second_agent().key()),
    ];
    bridge
        .state
        .mappings
        .insert(second_agent().key(), Mapping::default());
    let before = bridge.state.clone();
    assert!(
        bridge
            .attach_shared_chat(&"bb".repeat(16), &[app_npub(0x22)], &bindings)
            .await
            .is_err()
    );
    assert_eq!(bridge.state, before);
    bridge.state.mappings.clear();
    let mut invalid = bindings.clone();
    invalid[1].0 = "builder".into();
    assert!(
        bridge
            .attach_shared_chat(&"bb".repeat(16), &[app_npub(0x22)], &invalid)
            .await
            .is_err()
    );
    assert!(bridge.state.mappings.is_empty());
    drop(bridge);
    let mut bridge = fixture.attached().await;
    for mapping in bridge.state.mappings.values() {
        assert!(mapping.seen_messages.contains(&"old-group".into()));
        assert!(mapping.attached_after.is_some());
        assert!(mapping.output_initialised && !mapping.final_ids.is_empty());
    }
    bridge.recover_inbound().await.unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(bridge.state.outbox.is_empty());
    assert!(fixture.root.path().is_dir());
    assert_eq!(fixture.rows.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_chat_source_drift_cannot_create_a_private_mirror() {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    let mut replacement = agent();
    replacement.agent_session.as_mut().unwrap().source = "different-source".into();
    let before = bridge.state.mappings.len();
    let _ = bridge
        .reconcile(vec![replacement.clone(), second_agent()])
        .await;
    assert_eq!(bridge.state.mappings.len(), before);
    assert!(!bridge.state.mappings.contains_key(&replacement.key()));
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["type"] == "group_create_with_admins")
    );
}

async fn assert_changed_shared_identity_is_held(
    empty_outer: bool,
    remove_service: bool,
    duplicate: bool,
) {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    let retained = bridge.state.mappings[&agent().key()].clone();
    let mut replacement = agent();
    replacement.agent_session.as_mut().unwrap().source = "changed-native-source".into();
    if empty_outer {
        replacement.agent.clear();
    }
    let mut snapshot = vec![replacement.clone(), second_agent()];
    if duplicate {
        snapshot.push(agent());
    }
    *fixture.agents.lock().unwrap() = snapshot.clone();
    if remove_service {
        // A working legacy reader is essential here: reader failure must not
        // accidentally hide the private-discovery bug after config removal.
        bridge.config.opencode_service_file = None;
        export_script(
            &bridge.config.opencode_bin,
            "confidential shared transcript",
            "legacy-readable",
        );
        let history = herdr_white_noise_bridge::output::read_history(
            &bridge.config,
            replacement.agent_session.as_ref().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(history.finals[0].text, "confidential shared transcript");
    } else {
        fixture
            .histories
            .lock()
            .unwrap()
            .get_mut("ses_1")
            .unwrap()
            .push(final_message(
                "confidential",
                "confidential shared transcript",
            ));
    }
    bridge.reconcile(snapshot).await.unwrap();
    // Probe the exact private destination used by the fixture create response.
    // With correct isolation it is unmapped, so even the owner cannot replay
    // shared native history there. No real chat or account is ever opened.
    let mut last = message(HEX, "private-history-probe", "/last");
    last["group_id_hex"] = json!("ee".repeat(16));
    last["message"]["recorded_at"] = json!(0);
    bridge.inbound(&last).await.unwrap();
    let replacement_mapping = bridge.state.mappings.contains_key(&replacement.key());
    let private_outbox = bridge
        .state
        .outbox
        .iter()
        .any(|item| item.group != "bb".repeat(16));
    bridge.flush().await.unwrap();
    let calls = fixture.calls.lock().unwrap();
    let private_create = calls
        .iter()
        .any(|call| call["type"] == "group_create_with_admins");
    let private_send = calls.iter().any(|call| {
        matches!(
            call["type"].as_str(),
            Some("send_final" | "send_media" | "edit_message")
        ) && call["group_id_hex"] != "bb".repeat(16)
    });
    assert!(
        !replacement_mapping && !private_create && !private_outbox && !private_send,
        "Shared identity escaped: replacement_mapping={replacement_mapping}, private_create={private_create}, private_outbox={private_outbox}, private_send={private_send}"
    );
    let current = &bridge.state.mappings[&agent().key()];
    assert_eq!(current.group_id_hex, retained.group_id_hex);
    assert_eq!(current.shared_alias, retained.shared_alias);
    assert_eq!(current.agent_record, retained.agent_record);
    assert_eq!(current.seen_messages, retained.seen_messages);
    assert_eq!(bridge.state.mappings.len(), 2);
}

#[tokio::test]
async fn shared_chat_source_guard_holds_with_missing_outer_agent_metadata() {
    assert_changed_shared_identity_is_held(true, false, false).await;
}

#[tokio::test]
async fn shared_chat_source_guard_holds_without_service_registration_and_with_working_reader() {
    assert_changed_shared_identity_is_held(false, true, false).await;
}

#[tokio::test]
async fn shared_chat_source_guard_holds_ambiguous_native_ids_with_missing_outer_metadata() {
    assert_changed_shared_identity_is_held(true, false, true).await;
}

#[tokio::test]
async fn legacy_shared_chat_busy_and_idle_peers_both_refuse_invocation() {
    for status in ["working", "blocked"] {
        let fixture = Fixture::new().await;
        let mut bridge = fixture.attached().await;
        {
            let mut agents = fixture.agents.lock().unwrap();
            agents[0].agent_status = status.into();
            agents[0].state_change_seq += 1;
        }
        bridge.reconcile(fixture.snapshot()).await.unwrap();
        let event = message(&"22".repeat(32), "busy-fanout", "@builder @reviewer work");
        bridge.inbound(&event).await.unwrap();
        bridge.inbound(&event).await.unwrap();
        let posts = fixture.posts.lock().unwrap();
        assert_eq!(
            posts
                .iter()
                .filter(|(session, _)| session == "ses_2")
                .count(),
            0,
            "Legacy fanout is disabled regardless of peer status {status}"
        );
        // Neither idle nor working native sessions may bypass strict room policy.
        assert_eq!(
            posts
                .iter()
                .filter(|(session, _)| session == "ses_1")
                .count(),
            0
        );
        assert!(
            bridge
                .state
                .mappings
                .values()
                .all(|m| m.room_quarantined.is_some())
        );
    }
}

#[tokio::test]
async fn legacy_shared_chat_reader_recovery_cannot_restore_alias_invocation() {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    fixture.failed_reads.lock().unwrap().push("ses_1".into());
    let event = message(&"22".repeat(32), "reader-fanout", "@builder @reviewer work");
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        fixture
            .posts
            .lock()
            .unwrap()
            .iter()
            .map(|(session, _)| session.clone())
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    assert!(
        bridge.state.mappings[&agent().key()]
            .seen_messages
            .contains(&"reader-fanout".into())
    );
    assert!(
        bridge.state.mappings[&second_agent().key()]
            .room_quarantined
            .is_some()
    );
    bridge.inbound(&event).await.unwrap();
    assert!(fixture.posts.lock().unwrap().is_empty());
    fixture.failed_reads.lock().unwrap().clear();
    bridge.inbound(&event).await.unwrap();
    bridge.inbound(&event).await.unwrap();
    assert_eq!(
        fixture
            .posts
            .lock()
            .unwrap()
            .iter()
            .map(|(session, _)| session.clone())
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
}

#[tokio::test]
async fn legacy_shared_chat_quarantine_preserves_independent_private_delivery() {
    let mut fixture = Fixture::new().await;
    fixture.config.edit_responses = false;
    let mut bridge = fixture.attached().await;
    let mut private = agent();
    private.pane_id = "w1:p3".into();
    private.terminal_id = "term3".into();
    private.agent_session.as_mut().unwrap().value = "ses_private".into();
    fixture.agents.lock().unwrap().push(private.clone());
    fixture
        .histories
        .lock()
        .unwrap()
        .insert("ses_private".into(), vec![]);
    let mut mapping = activity_mapping(private.clone(), &"dd".repeat(16));
    mapping.group_name_locked = true;
    bridge.state.mappings.insert(private.key(), mapping);
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge
        .inbound(&message(&"22".repeat(32), "shared-build", "@builder build"))
        .await
        .unwrap();
    let mut private_input = message(HEX, "private-input", "private work");
    private_input["group_id_hex"] = json!("dd".repeat(16));
    bridge.inbound(&private_input).await.unwrap();
    bridge
        .inbound(&message(
            &"33".repeat(32),
            "shared-review",
            "@reviewer review",
        ))
        .await
        .unwrap();
    let posts = fixture.posts.lock().unwrap().clone();
    assert_eq!(
        posts
            .iter()
            .map(|(session, _)| session.as_str())
            .collect::<Vec<_>>(),
        ["ses_private"]
    );
    for (index, (session, post)) in posts.iter().enumerate() {
        let mut reply = final_message(
            &format!("simultaneous-{index}"),
            &format!("reply only for {session}"),
        );
        reply["parentID"] = post["id"].clone();
        fixture
            .histories
            .lock()
            .unwrap()
            .get_mut(session)
            .unwrap()
            .push(reply);
    }
    drop(bridge);
    // Only the unrelated private binding may deliver a response.
    let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
    bridge.reconcile(fixture.snapshot()).await.unwrap();
    bridge.flush().await.unwrap();
    let calls = fixture.calls.lock().unwrap();
    {
        let (session, group) = ("ses_private", "dd".repeat(16));
        let text = format!("reply only for {session}");
        let deliveries: Vec<_> = calls
            .iter()
            .filter(|call| {
                call["type"] == "send_final"
                    && call["text"]
                        .as_str()
                        .is_some_and(|body| body.ends_with(&text))
            })
            .collect();
        assert_eq!(
            deliveries.len(),
            1,
            "{session} must have exactly one confirmed delivery"
        );
        assert_eq!(deliveries[0]["group_id_hex"], group);
        assert_eq!(deliveries[0]["text"], text);
    }
    assert!(
        !calls
            .iter()
            .any(|call| call["method"] == "agent.prompt"
                || call["type"] == "group_create_with_admins")
    );
}

#[tokio::test]
async fn shared_chat_human_invocation_never_grants_native_tool_approval() {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    let permission = herdr_white_noise_bridge::output::permission::from_response(&json!({"data":[{
        "id":"per_fixture","sessionID":"ses_1","action":"shell","resources":["git status"],
        "source":{"type":"tool","messageID":"msg_fixture","id":"call_fixture"},"metadata":{"command":"git status"}
    }]}), "ses_1").unwrap().unwrap();
    bridge
        .state
        .mappings
        .get_mut(&agent().key())
        .unwrap()
        .permission = Some(permission);
    for (sender, id) in [
        ("22".repeat(32), "member-approval"),
        (HEX.to_owned(), "owner-approval"),
    ] {
        bridge
            .inbound(&message(&sender, id, "@builder 1"))
            .await
            .unwrap();
    }
    assert!(fixture.posts.lock().unwrap().is_empty());
    assert!(
        bridge.state.mappings[&agent().key()]
            .room_quarantined
            .is_some()
    );
    assert!(
        bridge.state.mappings[&agent().key()]
            .permission_attempts
            .is_empty()
    );
}

#[tokio::test]
async fn shared_chat_global_recent_does_not_replay_dedicated_shared_history() {
    let fixture = Fixture::new().await;
    let mut bridge = fixture.attached().await;
    bridge.state.mappings.insert(
        "control".into(),
        Mapping {
            control: true,
            active: true,
            group_id_hex: Some("dd".repeat(16)),
            ..Mapping::default()
        },
    );
    bridge.poll_sessions(fixture.snapshot()).await.unwrap();
    let mut event = message(HEX, "recent-shared-exclusion", "/recent");
    event["group_id_hex"] = json!("dd".repeat(16));
    bridge.inbound(&event).await.unwrap();
    assert_eq!(bridge.state.history_requests.len(), 1);
    assert!(
        bridge.state.history_requests[0].targets.is_empty(),
        "Private global history controls must not backfill shared groups"
    );
}

#[test]
fn shared_chat_completed_progress_keeps_its_label_and_checklist() {
    let mut state = BridgeState::with_mapping("agent", "group".into());
    state.mappings.get_mut("agent").unwrap().shared_alias = Some("builder".into());
    state.update_response("agent", "group", "> Working...\n- [x] Implement", false);
    state.outbox[0].response.as_mut().unwrap().target = Some("progress".into());
    state.complete_progress("agent");
    assert_eq!(
        state.outbox[0].text,
        "**@builder**\n> Agent turn finished\n- [x] Implement"
    );
}

fn moved_native_agent() -> Agent {
    let mut moved = agent();
    moved.agent_session.as_mut().unwrap().source = "changed-attachment-source".into();
    moved.terminal_id = "new-terminal".into();
    moved.pane_id = "w1:p-moved".into();
    moved
}

#[tokio::test]
async fn shared_chat_attachment_rejects_private_rebinding_of_shared_native_identity() {
    for remove_service in [false, true] {
        let fixture = Fixture::new().await;
        let mut bridge = fixture.attached().await;
        let moved = moved_native_agent();
        fixture.agents.lock().unwrap()[0] = moved.clone();
        fixture.info.lock().unwrap()["group_id_hex"] = json!("ee".repeat(16));
        if remove_service {
            bridge.config.opencode_service_file = None;
            // Missing delivery labels/config must not erase ownership, and the
            // reader must work so an unrelated read error cannot mask exposure.
            bridge
                .state
                .mappings
                .get_mut(&agent().key())
                .unwrap()
                .agent_record
                .as_mut()
                .unwrap()
                .agent
                .clear();
            export_script(
                &bridge.config.opencode_bin,
                "confidential shared history",
                "readable-shared",
            );
            let history = herdr_white_noise_bridge::output::read_history(
                &bridge.config,
                moved.agent_session.as_ref().unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(history.finals[0].text, "confidential shared history");
        }
        bridge.save().unwrap();
        let before = bridge.state.clone();
        let result = bridge
            .attach_chat(&moved.key(), &"ee".repeat(16), &app_npub(0x44))
            .await;
        let mut last = inbound(HEX, "duplicate-private-last");
        last["group_id_hex"] = json!("ee".repeat(16));
        last["message"]["text"] = json!("/last");
        bridge.inbound(&last).await.unwrap();
        let private_outbox = bridge
            .state
            .outbox
            .iter()
            .any(|item| item.group == "ee".repeat(16));
        bridge.flush().await.unwrap();
        let private_send = fixture.calls.lock().unwrap().iter().any(|call| {
            call["group_id_hex"] == "ee".repeat(16)
                && matches!(
                    call["type"].as_str(),
                    Some("send_final" | "send_media" | "edit_message" | "group_create_with_admins")
                )
        });
        assert!(
            result.is_err()
                && !private_outbox
                && !private_send
                && bridge.state.mappings == before.mappings,
            "Duplicate private attachment escaped: accepted={}, private_outbox={private_outbox}, private_send={private_send}, mappings_changed={}",
            result.is_ok(),
            bridge.state.mappings != before.mappings
        );
        // Polling clocks are deliberately not persisted. Compare the complete
        // durable projection, as well as the unchanged in-memory mapping above.
        assert_eq!(
            serde_json::to_value(
                BridgeState::load(&bridge.config.state_path)
                    .unwrap()
                    .mappings
            )
            .unwrap(),
            serde_json::to_value(&before.mappings).unwrap()
        );
        // A genuinely fresh native identity can still use the original private
        // attachment path; rejecting a collision must not reserve the new room.
        let mut fresh = moved.clone();
        fresh.agent_session.as_mut().unwrap().value = "ses_fresh_private".into();
        fixture.agents.lock().unwrap()[0] = fresh.clone();
        fixture
            .histories
            .lock()
            .unwrap()
            .insert("ses_fresh_private".into(), vec![]);
        if remove_service {
            export_script(
                &bridge.config.opencode_bin,
                "fresh private history",
                "fresh-private-baseline",
            );
        }
        bridge
            .attach_chat(&fresh.key(), &"ee".repeat(16), &app_npub(0x44))
            .await
            .unwrap();
        assert_eq!(bridge.state.mappings.len(), before.mappings.len() + 1);
        assert!(bridge.state.mappings[&fresh.key()].shared_alias.is_none());
        for (key, mapping) in before.mappings {
            assert_eq!(bridge.state.mappings[&key], mapping);
        }
    }
}

#[tokio::test]
async fn shared_chat_attachment_rejects_shared_rebinding_of_private_native_identity_atomically() {
    for missing_record in [false, true] {
        let fixture = Fixture::new().await;
        let mut bridge = Bridge::open(fixture.config.clone()).unwrap();
        bridge
            .attach_chat(&agent().key(), &"bb".repeat(16), &app_npub(0x22))
            .await
            .unwrap();
        let retained = bridge.state.mappings.get_mut(&agent().key()).unwrap();
        if missing_record {
            // The retained source-inclusive native key remains authoritative
            // when an older mapping has no saved Agent record at all.
            retained.agent_record = None;
        } else {
            retained.agent_record.as_mut().unwrap().agent.clear();
        }
        bridge.save().unwrap();
        let before = bridge.state.clone();
        let moved = moved_native_agent();
        fixture.agents.lock().unwrap()[0] = moved.clone();
        fixture.info.lock().unwrap()["group_id_hex"] = json!("ee".repeat(16));
        let result = bridge
            .attach_shared_chat(
                &"ee".repeat(16),
                &[app_npub(0x33), app_npub(0x44)],
                &[
                    ("fresh-first".into(), second_agent().key()),
                    ("colliding-second".into(), moved.key()),
                ],
            )
            .await;
        assert!(
            result.is_err() && bridge.state == before,
            "Shared attachment must roll back the complete roster on retained native ownership: accepted={}, state_changed={}",
            result.is_ok(),
            bridge.state != before
        );
        assert_eq!(
            BridgeState::load(&bridge.config.state_path).unwrap(),
            before
        );
        assert!(!fixture.calls.lock().unwrap().iter().any(|call| matches!(
            call["type"].as_str(),
            Some("send_final" | "send_media" | "edit_message" | "group_create_with_admins")
        )));
        // The first prepared, genuinely fresh session was not partially bound.
        // It can be explicitly bound alone while the retained private mapping
        // remains byte-for-byte unchanged and no old native output is replayed.
        bridge
            .attach_shared_chat(
                &"ee".repeat(16),
                &[app_npub(0x33), app_npub(0x44)],
                &[("fresh".into(), second_agent().key())],
            )
            .await
            .unwrap();
        assert_eq!(bridge.state.mappings.len(), 2);
        assert_eq!(
            bridge.state.mappings[&agent().key()],
            before.mappings[&agent().key()]
        );
        assert_eq!(
            bridge.state.mappings[&second_agent().key()].group_id_hex,
            Some("ee".repeat(16))
        );
        assert!(bridge.state.outbox.is_empty());
    }
}
