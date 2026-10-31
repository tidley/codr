use bech32::ToBase32;
use herdr_white_noise_bridge::{
    BridgeConfig, bridge::Bridge, herdr::Agent, state::Mapping, wire::read_line,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{io::AsyncWriteExt, net::UnixListener};

fn config(root: &Path) -> BridgeConfig {
    // tempfile's macOS root sits behind the /var symlink; launch roots must be real paths.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    serde_json::from_value(json!({"phone_npub":bech32::encode("npub",[0x11u8;32].to_base32(),bech32::Variant::Bech32).unwrap(),"account_id_hex":"aa".repeat(32),"herdr_socket":root.join("h.sock"),"wn_agent_socket":root.join("w.sock"),"state_path":root.join("state.json"),"launch_root":root.join("projects"),"opencode_bin":root.join("opencode"),"codex_home":root.join("codex"),"auto_forwarding":false})).unwrap()
}
fn event(id: &str, text: &str) -> Value {
    json!({"marmot_agent_control":"marmot.agent-control.v2","type":"inbound_message","account_id_hex":"aa".repeat(32),"group_id_hex":"bb".repeat(16),"message":{"message_id_hex":id,"sender":{"account_id_hex":"11".repeat(32)},"text":text}})
}
fn source(config: &BridgeConfig, kind: &str) -> Agent {
    serde_json::from_value(json!({"pane_id":"w1:p1","terminal_id":"t1","tab_id":"w1:t1","workspace_id":"w1","agent":kind,"agent_status":"idle","cwd":config.launch_root.join("existing"),"agent_session":{"source":kind,"agent":kind,"kind":"id","value":"ses_existing"},"terminal_title_stripped":null})).unwrap()
}
fn bridge(config: &BridgeConfig, source: Option<Agent>) -> (Bridge, String) {
    std::fs::create_dir_all(config.launch_root.join("existing")).unwrap();
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let key = source
        .as_ref()
        .map(Agent::key)
        .unwrap_or_else(|| "source".into());
    bridge.state.mappings.insert(
        key.clone(),
        Mapping {
            group_id_hex: Some("bb".repeat(16)),
            agent_record: source,
            active: true,
            ..Mapping::default()
        },
    );
    bridge.save().unwrap();
    (bridge, key)
}
fn last(bridge: &Bridge) -> &str {
    &bridge.state.outbox.last().unwrap().text
}

async fn model_code(bridge: &mut Bridge, key: &str) -> String {
    let draft = bridge.state.mappings[key].launch_draft.as_ref().unwrap();
    if draft.step == herdr_white_noise_bridge::launch_flow::Step::ModelGroup {
        let index = draft
            .model_groups
            .iter()
            .position(|group| group.label == "test models")
            .unwrap()
            + 1
            + usize::from(
                draft
                    .models
                    .first()
                    .is_some_and(|model| model.id.is_empty()),
            );
        let code = draft.code.clone();
        bridge
            .inbound(&event(
                &format!("provider-{code}"),
                &format!("/model {code} {index}"),
            ))
            .await
            .unwrap();
    }
    draft_code(bridge, key)
}

#[tokio::test]
async fn folder_pages_survive_restart_refresh_and_select_existing_projects() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, _) = host(&config, None).await;
    for i in 1..=400 {
        std::fs::create_dir(config.launch_root.join(format!("project-{i:03}"))).unwrap();
    }
    let existing_file = config.launch_root.join("project-400/keep.txt");
    std::fs::write(&existing_file, "existing project contents").unwrap();
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folders", "ls")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .folder_choices
            .len(),
        401
    );
    bridge.inbound(&event("next-page", "next")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        1
    );

    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&event("last-page", "next")).await.unwrap();
    assert!(last(&bridge).contains("401. project-400"));
    assert!(!last(&bridge).contains("Next page:"));
    bridge.inbound(&event("past-end", "next")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        2
    );
    bridge
        .inbound(&event("previous-page", "previous"))
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        1
    );
    bridge
        .inbound(&event("hidden-choice", "401"))
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .step,
        herdr_white_noise_bridge::launch_flow::Step::FolderList
    );
    bridge.inbound(&event("refresh", "ls")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        0
    );
    bridge
        .inbound(&event("first-page-previous", "previous"))
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        0
    );
    bridge
        .inbound(&event("second-again", "next"))
        .await
        .unwrap();
    bridge.inbound(&event("last-again", "next")).await.unwrap();
    bridge
        .inbound(&event("pick-existing", "401"))
        .await
        .unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    assert_eq!(
        draft.step,
        herdr_white_noise_bridge::launch_flow::Step::Agent
    );
    assert_eq!(draft.page, 0);
    assert_eq!(
        draft.directory.as_deref(),
        Some(existing_file.parent().unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(existing_file).unwrap(),
        "existing project contents"
    );
    assert!(bridge.state.mappings[&key].launch.is_none());
    server.abort();
}

#[tokio::test]
async fn folder_reuses_the_existing_conversation_without_starting_or_remapping() {
    for list in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let agent = source(&config, "opencode");
        let (mut bridge, key) = bridge(&config, None);
        let existing_key = agent.key();
        let mapping = Mapping {
            group_id_hex: Some("cc".repeat(16)),
            group_name: Some("Existing project chat".into()),
            agent_record: Some(agent.clone()),
            active: true,
            ..Mapping::default()
        };
        bridge
            .state
            .mappings
            .insert(existing_key.clone(), mapping.clone());
        bridge.save().unwrap();
        let (server, calls) = host(&config, Some(agent)).await;
        bridge.inbound(&event("open", "/new")).await.unwrap();
        bridge.inbound(&event("workspace", "new")).await.unwrap();
        if list {
            bridge.inbound(&event("folders", "ls")).await.unwrap();
        }
        bridge
            .inbound(&event("folder", if list { "1" } else { "existing" }))
            .await
            .unwrap();
        assert!(last(&bridge).contains("Existing project chat"));
        assert!(last(&bridge).contains("No new session"));
        assert!(bridge.state.mappings[&key].launch_draft.is_none());
        assert!(bridge.state.mappings[&key].launch.is_none());
        assert_eq!(bridge.state.mappings[&existing_key], mapping);
        assert!(calls.lock().unwrap().iter().all(|call| matches!(
            call["method"].as_str(),
            Some("workspace.list" | "session.snapshot" | "agent.get")
        )));
        server.abort();
    }
}

#[tokio::test]
async fn folder_filter_preserves_pagination_restart_and_selection() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, _) = host(&config, None).await;
    for i in 0..405 {
        std::fs::create_dir(config.launch_root.join(format!("Match-{i:03}"))).unwrap();
    }
    std::fs::create_dir(config.launch_root.join("other-folder")).unwrap();
    let retained = config.launch_root.join("Match-404/keep.txt");
    std::fs::write(&retained, "Keep existing contents").unwrap();
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("filter", "LS mAtCh")).await.unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    assert_eq!(
        draft.step,
        herdr_white_noise_bridge::launch_flow::Step::FolderList
    );
    assert_eq!(draft.folder_choices.len(), 405);
    assert!(last(&bridge).contains("page 1 of 3"));
    assert!(!last(&bridge).contains("other-folder"));
    bridge.inbound(&event("next", "next")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        1
    );
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&event("last-page", "next")).await.unwrap();
    assert!(last(&bridge).contains("405. Match-404"));
    bridge.inbound(&event("hidden-pick", "1")).await.unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    assert_eq!(
        draft.step,
        herdr_white_noise_bridge::launch_flow::Step::FolderList
    );
    assert_eq!(draft.page, 2);
    bridge.inbound(&event("pick", "405")).await.unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    assert_eq!(
        draft.step,
        herdr_white_noise_bridge::launch_flow::Step::Agent
    );
    assert_eq!(draft.directory.as_deref(), retained.parent());
    assert_eq!(
        std::fs::read_to_string(retained).unwrap(),
        "Keep existing contents"
    );
    assert!(bridge.state.mappings[&key].launch.is_none());
    server.abort();
}

#[tokio::test]
async fn folder_filter_clears_resets_and_accepts_literal_navigation_names() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, _) = host(&config, None).await;
    for i in 0..205 {
        std::fs::create_dir(config.launch_root.join(format!("project-{i:03}"))).unwrap();
    }
    for name in ["next-project", "previous-project", "filter-project"] {
        std::fs::create_dir(config.launch_root.join(name)).unwrap();
    }
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("list", "ls")).await.unwrap();
    bridge.inbound(&event("next-page", "next")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .page,
        1
    );
    for (id, command, expected) in [
        ("next-name", "ls NEXT", "next-project"),
        ("previous-name", "ls previous", "previous-project"),
        ("filter-name", "ls filter", "filter-project"),
    ] {
        bridge.inbound(&event(id, command)).await.unwrap();
        let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
        assert_eq!(draft.page, 0);
        assert_eq!(draft.folder_choices, [expected]);
    }
    bridge
        .inbound(&event("no-matches", "ls missing-name"))
        .await
        .unwrap();
    assert!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .folder_choices
            .is_empty()
    );
    assert!(last(&bridge).contains("No matching folders"));
    bridge.inbound(&event("clear", "ls")).await.unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    assert!(draft.folder_choices.len() > 205);
    assert_eq!(draft.page, 0);
    assert!(last(&bridge).contains("page 1 of 2"));
    assert!(bridge.state.mappings[&key].launch.is_none());
    server.abort();
}

#[tokio::test]
async fn folder_existing_conversation_choices_survive_restart_and_reject_replacements() {
    for replaced in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let mut first = source(&config, "opencode");
        first.tab_label = Some("First conversation".into());
        let mut second = first.clone();
        second.agent_session.as_mut().unwrap().value = "ses_second".into();
        second.pane_id = "w1:p2".into();
        second.terminal_id = "t2".into();
        second.tab_label = Some("Second conversation".into());
        let (mut bridge, key) = bridge(&config, None);
        for (agent, name, group) in [(&first, "First chat", "cc"), (&second, "Second chat", "dd")] {
            bridge.state.mappings.insert(
                agent.key(),
                Mapping {
                    group_id_hex: Some(group.repeat(16)),
                    group_name: Some(name.into()),
                    agent_record: Some(agent.clone()),
                    active: true,
                    ..Mapping::default()
                },
            );
        }
        let workspaces = Arc::new(Mutex::new(vec![
            json!({"workspace_id":"w1","label":"Main","snapshot_agents":[first,second]}),
        ]));
        let (server, calls) = host_with_workspaces(&config, None, workspaces.clone()).await;
        bridge.inbound(&event("open", "/new")).await.unwrap();
        bridge.inbound(&event("workspace", "new")).await.unwrap();
        bridge.inbound(&event("folder", "existing")).await.unwrap();
        assert!(last(&bridge).contains("Choose an existing conversation"));
        assert!(last(&bridge).contains("First conversation"));
        assert!(last(&bridge).contains("Second conversation"));
        bridge.save().unwrap();
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        if replaced {
            workspaces.lock().unwrap()[0]["snapshot_agents"][1]["agent_session"]["value"] =
                json!("ses_replacement");
        }
        bridge.inbound(&event("pick", "2")).await.unwrap();
        if replaced {
            assert!(last(&bridge).contains("unavailable or changed"));
        } else {
            assert!(last(&bridge).contains("Second chat"));
            assert!(bridge.state.mappings[&key].launch_draft.is_none());
        }
        assert!(bridge.state.mappings[&key].launch.is_none());
        assert!(calls.lock().unwrap().iter().all(|call| matches!(
            call["method"].as_str(),
            Some("workspace.list" | "session.snapshot" | "agent.get")
        )));
        server.abort();
    }
}

#[tokio::test]
async fn folder_existing_history_is_not_replaced_when_herdr_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folder", "existing")).await.unwrap();
    assert!(last(&bridge).contains("Could not check existing conversations"));
    assert!(bridge.state.mappings[&key].launch.is_none());
    assert!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .directory
            .is_none()
    );
}

#[tokio::test]
async fn folder_reuses_native_identity_when_the_detected_backend_label_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let mut agent = source(&config, "opencode");
    agent.agent.clear();
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, Some(agent)).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folder", "existing")).await.unwrap();
    assert!(last(&bridge).contains("No new session"));
    assert!(bridge.state.mappings[&key].launch_draft.is_none());
    assert!(bridge.state.mappings[&key].launch.is_none());
    assert!(!calls.lock().unwrap().iter().any(|call| matches!(
        call["method"].as_str(),
        Some("workspace.create" | "tab.create" | "agent.start")
    )));
    server.abort();
}

#[tokio::test]
async fn folder_missing_directory_does_not_replace_a_retained_conversation() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, None);
    std::fs::rename(
        config.launch_root.join("existing"),
        config.launch_root.join("relocated"),
    )
    .unwrap();
    bridge.state.mappings.insert(
        agent.key(),
        Mapping {
            agent_record: Some(agent),
            group_id_hex: Some("cc".repeat(16)),
            active: false,
            ..Mapping::default()
        },
    );
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folder", "existing")).await.unwrap();
    assert!(last(&bridge).contains("folder for an existing conversation is missing"));
    assert!(bridge.state.mappings[&key].launch.is_none());
    assert!(!config.launch_root.join("existing").exists());
}

#[tokio::test]
async fn folder_retained_conversation_requires_reattachment_instead_of_replacement() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, None);
    bridge.state.mappings.insert(
        agent.key(),
        Mapping {
            agent_record: Some(agent),
            group_id_hex: Some("cc".repeat(16)),
            active: false,
            ..Mapping::default()
        },
    );
    let (server, calls) = host(&config, None).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folder", "existing")).await.unwrap();
    assert!(last(&bridge).contains("Reattach the original session"));
    assert!(bridge.state.mappings[&key].launch.is_none());
    assert!(!calls.lock().unwrap().iter().any(|call| matches!(
        call["method"].as_str(),
        Some("workspace.create" | "tab.create" | "agent.start")
    )));
    server.abort();
}

#[tokio::test]
async fn folder_rechecks_for_conversations_before_confirming_a_new_session() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let workspaces = Arc::new(Mutex::new(vec![
        json!({"workspace_id":"w1","label":"Main","snapshot_agents":[]}),
    ]));
    let (server, calls) = host_with_workspaces(&config, None, workspaces.clone()).await;
    new_model_preview(&mut bridge, &key).await;
    std::fs::create_dir(config.launch_root.join("my-idea")).unwrap();
    let mut agent = source(&config, "opencode");
    agent.cwd = config
        .launch_root
        .join("my-idea")
        .to_string_lossy()
        .into_owned();
    bridge.state.mappings.insert(
        agent.key(),
        Mapping {
            group_id_hex: Some("cc".repeat(16)),
            group_name: Some("Existing chat".into()),
            agent_record: Some(agent.clone()),
            active: true,
            ..Mapping::default()
        },
    );
    workspaces.lock().unwrap()[0]["snapshot_agents"] = json!([agent]);
    bridge
        .inbound(&event("folder-changed", "confirm"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("folder changed"));
    bridge.inbound(&event("confirm", "confirm")).await.unwrap();
    assert!(last(&bridge).contains("Existing chat"));
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Cancelled
    );
    assert!(!calls.lock().unwrap().iter().any(|call| matches!(
        call["method"].as_str(),
        Some("workspace.create" | "tab.create" | "agent.start")
    )));
    server.abort();
}

#[tokio::test]
async fn folder_menu_words_remain_valid_one_line_workspace_names() {
    for name in ["list", "pick"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let (mut bridge, key) = bridge(&config, None);
        bridge
            .inbound(&event(
                "one-line",
                &format!("/new {name} my-project opencode"),
            ))
            .await
            .unwrap();
        assert!(
            bridge.state.mappings[&key]
                .launch
                .as_ref()
                .is_some_and(|request| request.label == name)
        );
        assert!(last(&bridge).contains("Confirm within ten minutes"));
    }
}

#[tokio::test]
async fn even_one_provider_starts_with_default_or_provider_and_then_lists_every_model() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folder", "my-idea")).await.unwrap();
    let index = bridge.state.mappings[&key]
        .launch_draft
        .as_ref()
        .unwrap()
        .agents
        .iter()
        .position(|a| a == "opencode")
        .unwrap()
        + 1;
    bridge
        .inbound(&event("agent", &index.to_string()))
        .await
        .unwrap();
    assert!(!last(&bridge).contains("Use the backend default"));
    assert!(last(&bridge).ends_with("**0. Cancel**"));
    assert!(last(&bridge).contains("1. Backend default\n2. View test models"));
    bridge.inbound(&event("provider", "2")).await.unwrap();
    assert!(last(&bridge).contains("1. Model 1"));
    assert!(last(&bridge).contains("14. Model 14"));
    assert!(!last(&bridge).contains("page"));
    assert!(!last(&bridge).contains("Next"));
    let before = draft_code(&bridge, &key);
    bridge
        .inbound(&event("no-pagination", "next"))
        .await
        .unwrap();
    assert_eq!(draft_code(&bridge, &key), before);
    bridge.inbound(&event("select", "14")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch
            .as_ref()
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .id,
        "model-14"
    );
    assert!(!config.launch_root.join("my-idea").exists());
}

#[tokio::test]
async fn long_provider_lists_deliver_all_models_in_bounded_durable_messages() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    change_api_state(&config, |s| {
        s["models"]=json!((1..=400).map(|i|json!({"id":format!("model-{i}"),"providerID":"test","name":format!("模型 {i} {}","x".repeat(95)),"enabled":true})).collect::<Vec<_>>())
    });
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    bridge.inbound(&event("folder", "my-idea")).await.unwrap();
    let index = bridge.state.mappings[&key]
        .launch_draft
        .as_ref()
        .unwrap()
        .agents
        .iter()
        .position(|a| a == "opencode")
        .unwrap()
        + 1;
    bridge
        .inbound(&event("agent", &index.to_string()))
        .await
        .unwrap();
    let start = bridge.state.outbox.len();
    let select = event("provider", "2");
    bridge.inbound(&select).await.unwrap();
    let pieces = &bridge.state.outbox[start..];
    assert!(pieces.len() > 1);
    assert!(
        pieces
            .iter()
            .all(|piece| piece.manual && piece.text.len() <= 30000)
    );
    let text = pieces
        .iter()
        .map(|piece| piece.text.as_str())
        .collect::<String>();
    assert!(!text.contains("page"));
    assert!(text.contains("400. 模型 400"));
    assert!(text.contains("test/model-400"));
    let saved = bridge.state.outbox.clone();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&select).await.unwrap();
    assert_eq!(bridge.state.outbox, saved);
    bridge.inbound(&event("last-model", "400")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch
            .as_ref()
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .id,
        "model-400"
    );
    assert!(!config.launch_root.join("my-idea").exists());
}

#[tokio::test]
async fn account_model_groups_and_variants_are_separate_restart_safe_menus() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    change_api_state(&config, |s| {
        s["models"] = json!([
            {"id":"free","providerID":"opencode","name":"Free model","enabled":true,"cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}]},
            {"id":"gpt-codex","providerID":"openai","name":"GPT Codex","enabled":true,"cost":[],"variants":[{"id":"high"},{"id":"low"}]},
            {"id":"claude","providerID":"anthropic","name":"Claude","enabled":true,"cost":[]}
        ])
    });
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    assert!(last(&bridge).starts_with("Choose workspace\n"));
    assert!(last(&bridge).ends_with("**0. Cancel**"));
    bridge.inbound(&event("workspace", "3")).await.unwrap();
    assert_eq!(
        last(&bridge),
        "Folder name:\nReply with your chosen folder name.\n\n**0. Cancel**\n\n**`ls`** to list existing folders, or **`ls <part>`** to filter them."
    );
    bridge.inbound(&event("folder", "my-idea")).await.unwrap();
    assert!(last(&bridge).starts_with("Choose agent:\n"));
    assert!(last(&bridge).ends_with("**0. Cancel**"));
    let index = bridge.state.mappings[&key]
        .launch_draft
        .as_ref()
        .unwrap()
        .agents
        .iter()
        .position(|a| a == "opencode")
        .unwrap()
        + 1;
    bridge
        .inbound(&event("agent", &index.to_string()))
        .await
        .unwrap();
    assert!(last(&bridge).starts_with("Choose a model for OpenCode:\n"));
    assert!(last(&bridge).ends_with("**0. Cancel**"));
    for label in [
        "1. Backend default",
        "2. View OpenAI / Codex models",
        "3. View Claude models",
        "4. View Free models",
        "0. Cancel",
    ] {
        assert!(last(&bridge).contains(label), "{}", last(&bridge));
    }
    assert!(!last(&bridge).contains("page"));
    bridge.inbound(&event("openai", "2")).await.unwrap();
    assert!(last(&bridge).contains("1. GPT Codex"));
    assert!(last(&bridge).contains("Back: back"));
    assert!(!last(&bridge).contains("Menus expire"));
    assert!(!last(&bridge).contains("Reply with a listed number"));
    assert!(!last(&bridge).contains("Free model"));
    assert!(!last(&bridge).contains("#high"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&event("model", "1")).await.unwrap();
    assert!(last(&bridge).contains("Choose a variant"));
    assert!(last(&bridge).contains("Back: back"));
    assert!(!last(&bridge).contains("Menus expire"));
    assert!(!last(&bridge).contains("[4/4]"));
    assert!(!last(&bridge).contains("Reply with a listed number"));
    assert!(last(&bridge).contains("2. high"));
    bridge.inbound(&event("variant", "2")).await.unwrap();
    let chosen = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .model
        .as_ref()
        .unwrap();
    assert_eq!(chosen.provider.as_deref(), Some("openai"));
    assert_eq!(chosen.variant.as_deref(), Some("high"));
    assert!(!config.launch_root.join("my-idea").exists());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    server.abort();
}

#[tokio::test]
async fn typing_the_displayed_project_name_confirms_but_wrong_or_unsafe_names_do_not() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    new_model_preview(&mut bridge, &key).await;
    assert!(last(&bridge).contains("Confirm within ten minutes"));
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: my-idea)"));
    bridge.inbound(&event("wrong", "other-name")).await.unwrap();
    assert!(last(&bridge).contains("my-idea"));
    assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
    for field in [
        "text_truncated",
        "attachments_truncated",
        "attachments",
        "media",
        "foreign",
    ] {
        let mut msg = event(field, "my-idea");
        match field {
            "foreign" => msg["message"]["sender"]["account_id_hex"] = json!("99".repeat(32)),
            "text_truncated" | "attachments_truncated" => msg["message"][field] = json!(true),
            _ => msg["message"][field] = json!([{"media_type":"image/png"}]),
        };
        bridge.inbound(&msg).await.unwrap();
        assert!(!config.launch_root.join("my-idea").exists());
    }
    let confirm = event("confirm", "my-idea");
    bridge.inbound(&confirm).await.unwrap();
    bridge.inbound(&confirm).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "workspace.create")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn grouped_models_support_back_cancel_and_provider_qualified_leaf_choices() {
    for option in [1, 3, 4] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        change_api_state(&config, |s| {
            s["models"] = json!([
                {"id":"shared","providerID":"openai","name":"OpenAI model","enabled":true,"cost":[],"variants":[{"id":"high"}]},
                {"id":"shared","providerID":"anthropic","name":"Claude model","enabled":true,"cost":[]},
                {"id":"free","providerID":"opencode","name":"Free model","enabled":true,"cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}]}
            ])
        });
        let (mut bridge, key) = bridge(&config, None);
        bridge.inbound(&event("open", "/new")).await.unwrap();
        bridge.inbound(&event("workspace", "new")).await.unwrap();
        bridge.inbound(&event("folder", "my-idea")).await.unwrap();
        let index = bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .agents
            .iter()
            .position(|a| a == "opencode")
            .unwrap()
            + 1;
        bridge
            .inbound(&event("agent", &index.to_string()))
            .await
            .unwrap();
        bridge.inbound(&event("group", "2")).await.unwrap();
        bridge.inbound(&event("base", "1")).await.unwrap();
        let old = draft_code(&bridge, &key);
        bridge.inbound(&event("back-models", "back")).await.unwrap();
        assert!(last(&bridge).contains("1. OpenAI model"));
        bridge.inbound(&event("back-groups", "back")).await.unwrap();
        assert!(last(&bridge).contains("View Claude models"));
        bridge
            .inbound(&event("stale", &format!("/model {old} 1")))
            .await
            .unwrap();
        assert!(bridge.state.mappings[&key].launch.is_none());
        bridge
            .inbound(&event("option", &option.to_string()))
            .await
            .unwrap();
        if option > 1 {
            bridge.inbound(&event("leaf", "1")).await.unwrap();
        }
        let model = bridge.state.mappings[&key]
            .launch
            .as_ref()
            .unwrap()
            .model
            .as_ref()
            .unwrap();
        match option {
            1 => assert!(model.id.is_empty()),
            3 => assert_eq!(model.provider.as_deref(), Some("anthropic")),
            4 => assert!(model.free),
            _ => unreachable!(),
        }
        assert!(!config.launch_root.join("my-idea").exists());
        bridge.inbound(&event("cancel", "0")).await.unwrap();
        assert_eq!(
            bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
            herdr_white_noise_bridge::launch::Phase::Cancelled
        );
    }
}

#[tokio::test]
async fn project_name_confirmation_rechecks_destination_renames_and_expiry() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let workspaces = Arc::new(Mutex::new(vec![
        json!({"workspace_id":"w1","label":"Main"}),
    ]));
    let (server, calls) = host_with_workspaces(&config, None, workspaces.clone()).await;
    existing_workspace_preview(&mut bridge, &key, 1).await;
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: side-project)"));
    bridge
        .inbound(&event("workspace-is-not-project", "Main"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("does not match"));
    assert!(!config.launch_root.join("side-project").exists());
    workspaces.lock().unwrap()[0]["label"] = json!("Renamed");
    bridge
        .inbound(&event("project-name", "side-project"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Renamed (w1)"));
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: side-project)"));
    assert!(!config.launch_root.join("side-project").exists());
    bridge
        .inbound(&event("wrong-name", "Renamed"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("does not match"));
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch
        .as_mut()
        .unwrap()
        .expires_at = 0;
    bridge
        .inbound(&event("expired", "side-project"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("expired"));
    assert!(!config.launch_root.join("side-project").exists());
    assert!(calls.lock().unwrap().iter().all(|c| matches!(
        c["method"].as_str(),
        Some("workspace.list" | "workspace.get")
    )));
    server.abort();
}

#[tokio::test]
async fn repository_folder_name_not_existing_workspace_name_confirms_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    existing_workspace_preview(&mut bridge, &key, 1).await;
    assert!(last(&bridge).contains("Workspace destination: Main (w1)"));
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: side-project)"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .inbound(&event("wrong-workspace-name", "Main"))
        .await
        .unwrap();
    assert!(!config.launch_root.join("side-project").exists());
    let confirm = event("confirm-project", "side-project");
    bridge.inbound(&confirm).await.unwrap();
    bridge.inbound(&confirm).await.unwrap();
    assert!(config.launch_root.join("side-project").is_dir());
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().filter(|c| c["method"] == "tab.create").count(),
        1
    );
    assert!(!calls.iter().any(|c| c["method"] == "workspace.create"));
    server.abort();
}

#[tokio::test]
async fn confirmation_uses_the_exact_dotted_folder_basename_not_a_workspace_label() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, _) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    bridge
        .inbound(&event(
            "propose",
            "/new workspace-label parent/repo.v2 opencode",
        ))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: repo.v2)"));
    bridge
        .inbound(&event("wrong-label", "workspace-label"))
        .await
        .unwrap();
    assert!(!config.launch_root.join("parent/repo.v2").exists());
    bridge
        .inbound(&event("confirm-folder", "repo.v2"))
        .await
        .unwrap();
    assert!(config.launch_root.join("parent/repo.v2").is_dir());
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "workspace.create")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn a_zero_named_repository_keeps_zero_as_cancel_not_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    bridge
        .inbound(&event("propose-zero", "/new workspace-label 0 opencode"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: /new confirm)"));
    bridge.inbound(&event("cancel-zero", "0")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Cancelled
    );
    assert!(!config.launch_root.join("0").exists());
}

#[tokio::test]
async fn an_unrelated_name_after_expiry_releases_the_confirmation_instead_of_trapping_chat() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    new_model_preview(&mut bridge, &key).await;
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch
        .as_mut()
        .unwrap()
        .expires_at = 0;
    bridge
        .inbound(&event("expired-wrong-name", "some ordinary message"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("expired"));
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Cancelled
    );
    assert!(!config.launch_root.join("my-idea").exists());
    let before = bridge.state.outbox.len();
    // The fake bridge has no live connection; a later normal prompt follows the normal defer path, not the expired menu.
    assert!(
        bridge
            .inbound(&event("normal", "another ordinary message"))
            .await
            .is_err()
    );
    assert_eq!(bridge.state.outbox.len(), before);
}

#[tokio::test]
async fn saved_free_model_selections_from_before_pricing_metadata_still_confirm() {
    for switch in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        change_api_state(&config, |s| {
            s["models"][1]["cost"] = json!([{"input":0,"output":0,"cache":{"read":0,"write":0}}])
        });
        let agent = source(&config, "opencode");
        let (mut bridge, key) = bridge(&config, Some(agent.clone()));
        let (server, calls) = host(&config, Some(agent)).await;
        if switch {
            bridge.inbound(&event("open", "/model")).await.unwrap();
            // Explicit free pricing produces two provider groups; choose the Free group then its one model.
            bridge.inbound(&event("free", "1")).await.unwrap();
            bridge.inbound(&event("model", "1")).await.unwrap();
        } else {
            // Simulate an old flat draft to generate a request without changing the original helper's indices.
            change_api_state(&config, |s| s["models"][1]["cost"] = json!([]));
            new_model_preview(&mut bridge, &key).await;
            change_api_state(&config, |s| {
                s["models"][1]["cost"] =
                    json!([{"input":0,"output":0,"cache":{"read":0,"write":0}}])
            });
        }
        bridge.save().unwrap();
        drop(bridge);
        let mut saved: Value =
            serde_json::from_slice(&std::fs::read(&config.state_path).unwrap()).unwrap();
        let field = if switch {
            &mut saved["mappings"][&key]["launch_draft"]["selected"]
        } else {
            &mut saved["mappings"][&key]["launch"]["model"]
        };
        field.as_object_mut().unwrap().remove("free");
        std::fs::write(&config.state_path, serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge
            .inbound(&event(
                "confirm",
                if switch { "/model confirm" } else { "my-idea" },
            ))
            .await
            .unwrap();
        if switch {
            assert!(last(&bridge).contains("Model changed"));
        } else {
            assert_eq!(
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c["method"] == "workspace.create")
                    .count(),
                1
            );
        }
        server.abort();
    }
}

#[tokio::test]
async fn location_steps_are_tidy_and_new_workspace_is_the_last_numbered_choice() {
    use herdr_white_noise_bridge::launch_flow::Step;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    let text = last(&bridge);
    assert!(text.starts_with("Choose workspace\n"));
    assert!(text.ends_with("**0. Cancel**"));
    assert!(!text.contains("page 1 of 1"));
    assert!(
        text.contains("\n\n**0. Cancel**"),
        "Markdown must not renumber the cancellation option"
    );
    assert!(
        text.contains("\n3. Create new workspace\n\n**0. Cancel**"),
        "The new-workspace number is independent of the current page"
    );
    assert!(text.find("1. Main").unwrap() < text.find("2. Other").unwrap());
    assert!(text.find("2. Other").unwrap() < text.find("3. Create new workspace").unwrap());
    assert!(text.find("3. Create new workspace").unwrap() < text.find("0. Cancel").unwrap());
    bridge.inbound(&event("new-workspace", "3")).await.unwrap();
    assert!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .target_workspace
            .is_none()
    );
    assert_eq!(
        last(&bridge),
        "Folder name:\nReply with your chosen folder name.\n\n**0. Cancel**\n\n**`ls`** to list existing folders, or **`ls <part>`** to filter them."
    );
    bridge
        .inbound(&event("folder", "side-project"))
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .step,
        Step::Agent
    );
    assert!(last(&bridge).contains("0. Cancel"));
    assert!(last(&bridge).starts_with("Choose agent:\n"));
    assert!(last(&bridge).ends_with("**0. Cancel**"));
    assert!(!config.launch_root.join("side-project").exists());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    server.abort();
}

#[tokio::test]
async fn slash_cancel_resets_control_chat_setup_and_pending_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    new_model_preview(&mut bridge, &key).await;
    bridge.inbound(&event("restart", "/new")).await.unwrap();
    assert!(bridge.state.mappings[&key].launch_draft.is_some());
    bridge
        .inbound(&event("soft-reset", "/cancel"))
        .await
        .unwrap();
    drop(bridge);
    let bridge = Bridge::open(config).unwrap();
    assert!(bridge.state.mappings[&key].launch_draft.is_none());
    assert_ne!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Awaiting
    );
}

#[tokio::test]
async fn zero_cancels_each_setup_menu_and_pending_launch_without_touching_agent_state() {
    use herdr_white_noise_bridge::launch_flow::Step;
    for step in [
        Step::Workspace,
        Step::Folder,
        Step::Agent,
        Step::Model,
        Step::ModelGroup,
        Step::Variant,
        Step::ModelConfirm,
    ] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let (mut bridge, key) = bridge(&config, None);
        let (server, calls) = host(&config, None).await;
        new_model_preview(&mut bridge, &key).await;
        let request = bridge.state.mappings[&key].launch.clone();
        bridge.inbound(&event("restart", "/new")).await.unwrap();
        let mut draft = bridge.state.mappings[&key].launch_draft.clone().unwrap();
        draft.step = step;
        draft.kind = Some("opencode".into());
        draft.selected = Some(herdr_white_noise_bridge::catalogue::Model::default_choice());
        bridge.state.mappings.get_mut(&key).unwrap().launch_draft = Some(draft);
        bridge.state.mappings.get_mut(&key).unwrap().launch = request;
        bridge.save().unwrap();
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.inbound(&event("cancel", "0")).await.unwrap();
        assert!(
            bridge.state.mappings[&key].launch_draft.is_none(),
            "{step:?}"
        );
        assert_eq!(
            bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
            herdr_white_noise_bridge::launch::Phase::Cancelled
        );
        assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| c["method"] == "workspace.list")
        );
        server.abort();
    }
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    new_model_preview(&mut bridge, &key).await;
    assert!(last(&bridge).contains("0. Cancel"));
    bridge.inbound(&event("cancel-launch", "0")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Cancelled
    );
    assert!(!config.launch_root.join("my-idea").exists());
}

#[tokio::test]
async fn plain_folder_answers_preserve_case_validate_paths_and_ignore_help() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "1")).await.unwrap(); // Empty/unavailable list: option 1 creates a workspace.
    let code = draft_code(&bridge, &key);
    bridge.inbound(&event("help", "/help")).await.unwrap();
    assert_eq!(draft_code(&bridge, &key), code);
    for (id, text) in [("traversal", "../outside"), ("spaces", "invalid folder")] {
        bridge.inbound(&event(id, text)).await.unwrap();
        assert_eq!(draft_code(&bridge, &key), code);
    }
    for field in [
        "text_truncated",
        "attachments_truncated",
        "attachments",
        "media",
    ] {
        let mut msg = event(field, "MyProject");
        msg["message"][field] = if field.ends_with("truncated") {
            json!(true)
        } else {
            json!([{"media_type":"image/png"}])
        };
        bridge.inbound(&msg).await.unwrap();
        assert_eq!(draft_code(&bridge, &key), code);
    }
    bridge.inbound(&event("valid", "MyProject")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .directory
            .as_ref()
            .unwrap(),
        &config.launch_root.join("MyProject")
    );
    assert!(!config.launch_root.join("MyProject").exists());
    assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
}

#[tokio::test]
async fn single_page_model_menus_have_no_page_counter_and_zero_cannot_clear_uncertain_work() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    change_api_state(&config, |s| {
        s["models"].as_array_mut().unwrap().truncate(1);
    });
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    let (server, _) = host(&config, Some(agent)).await;
    bridge.inbound(&event("open", "/model")).await.unwrap();
    assert!(!last(&bridge).contains("page"));
    assert!(last(&bridge).contains("0. Cancel"));
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .step = herdr_white_noise_bridge::launch_flow::Step::Uncertain;
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .expires_at = 0;
    let before = bridge.state.mappings[&key].launch_draft.clone();
    bridge.inbound(&event("cancel", "0")).await.unwrap();
    assert_eq!(bridge.state.mappings[&key].launch_draft, before);
    assert!(last(&bridge).contains("not confirmed"));
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    server.abort();
}

#[tokio::test]
async fn numbered_menu_replies_take_precedence_over_agent_decisions() {
    use herdr_white_noise_bridge::launch_flow::Step;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    let (server, calls) = host(&config, Some(agent)).await;
    let decision=serde_json::from_value(json!({"id":"stale-question","questions":[{"question":"Agent question","options":[{"label":"A"},{"label":"B"}]}],"announced":true})).unwrap();
    bridge.state.mappings.get_mut(&key).unwrap().decision = Some(decision);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    let old = bridge.state.mappings[&key].decision.clone();
    bridge.inbound(&event("choose", "1")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .step,
        Step::Folder
    );
    assert_eq!(bridge.state.mappings[&key].decision, old);
    assert!(!last(&bridge).contains("decision"));
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    server.abort();
}

#[tokio::test]
async fn an_invalid_workspace_shortcut_cannot_become_a_legacy_launch() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    let before = bridge.state.mappings[&key].launch_draft.clone();
    bridge
        .inbound(&event("invalid", "/new workspace opencode"))
        .await
        .unwrap();
    assert_eq!(bridge.state.mappings[&key].launch_draft, before);
    assert!(bridge.state.mappings[&key].launch.is_none());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    server.abort();
}

#[tokio::test]
async fn code_free_menu_choices_survive_restart_and_require_explicit_confirmation() {
    use herdr_white_noise_bridge::launch_flow::Step;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    assert!(!last(&bridge).contains(&code));
    assert!(last(&bridge).starts_with("Choose workspace\n"));
    assert!(last(&bridge).ends_with("**0. Cancel**"));
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .step,
        Step::Folder
    );
    bridge
        .inbound(&event("folder", "/new folder side-project"))
        .await
        .unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    let index = draft.agents.iter().position(|k| k == "opencode").unwrap() + 1;
    bridge
        .inbound(&event("agent", &index.to_string()))
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .step,
        Step::ModelGroup
    );
    bridge.inbound(&event("provider", "2")).await.unwrap();
    assert!(last(&bridge).contains("14. Model 14"));
    assert!(!last(&bridge).contains("page"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&event("select", "12")).await.unwrap();
    let request = bridge.state.mappings[&key].launch.as_ref().unwrap();
    assert_eq!(request.model.as_ref().unwrap().id, "model-12");
    assert!(!last(&bridge).contains(&request.code));
    assert!(last(&bridge).contains("Reply confirm, yes or y (or type: side-project)"));
    assert!(!config.launch_root.join("side-project").exists());
    bridge.inbound(&event("select", "12")).await.unwrap(); // Same message cannot become a prompt after the menu advances.
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    bridge
        .inbound(&event("confirm", "/new confirm"))
        .await
        .unwrap();
    bridge
        .inbound(&event("confirm-again", "/new confirm"))
        .await
        .unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "workspace.create")
            .count(),
        1
    );
    assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
    server.abort();
}

#[tokio::test]
async fn expired_folder_menu_is_cleared_before_later_messages_and_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge.inbound(&event("workspace", "new")).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .step,
        herdr_white_noise_bridge::launch_flow::Step::Folder
    );
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .expires_at = 0;
    bridge.save().unwrap();
    bridge
        .inbound(&event("expired-folder", "Unmute"))
        .await
        .unwrap();
    assert!(last(&bridge).contains("expired"));
    assert!(bridge.state.mappings[&key].launch_draft.is_none());
    assert!(
        bridge.state.mappings[&key]
            .seen_messages
            .iter()
            .any(|id| id == "expired-folder")
    );
    let count = bridge.state.outbox.len();
    bridge
        .inbound(&event("expired-folder", "Unmute"))
        .await
        .unwrap();
    assert_eq!(bridge.state.outbox.len(), count);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    assert!(bridge.state.mappings[&key].launch_draft.is_none());
    // No live agent is attached: normal text must take the normal defer path,
    // not produce another expired-menu reply or be mistaken for a folder.
    assert!(
        bridge
            .inbound(&event("ordinary", "Are you just sending this every time?"))
            .await
            .is_err()
    );
    assert_eq!(bridge.state.outbox.len(), count);
    assert!(!config.launch_root.join("Unmute").exists());
    assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
}

#[tokio::test]
async fn expired_menu_save_failure_keeps_the_draft_and_input_unconsumed() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .expires_at = 0;
    let before = serde_json::to_value(&bridge.state).unwrap();
    // Force the atomic state replacement to fail without changing forwarding state.
    std::fs::remove_file(&config.state_path).unwrap();
    std::fs::create_dir(&config.state_path).unwrap();
    assert!(
        bridge
            .inbound(&event("expired-save-fails", "1"))
            .await
            .is_err()
    );
    assert_eq!(serde_json::to_value(&bridge.state).unwrap(), before);
}

#[tokio::test]
async fn expired_invalid_and_unsafe_numbers_do_not_escape_the_menu() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    bridge.inbound(&event("open", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    for (i, field) in [
        "text_truncated",
        "attachments_truncated",
        "attachments",
        "media",
        "foreign",
    ]
    .iter()
    .enumerate()
    {
        let mut msg = event(&format!("unsafe-{i}"), "1");
        match *field {
            "foreign" => msg["message"]["sender"]["account_id_hex"] = json!("99".repeat(32)),
            "text_truncated" | "attachments_truncated" => msg["message"][field] = json!(true),
            _ => msg["message"][field] = json!([{"media_type":"image/png"}]),
        };
        bridge.inbound(&msg).await.unwrap();
        assert_eq!(draft_code(&bridge, &key), code);
    }
    bridge.inbound(&event("invalid", "999")).await.unwrap();
    assert!(last(&bridge).contains("listed workspace"));
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .expires_at = 0;
    bridge.inbound(&event("expired", "1")).await.unwrap();
    assert!(last(&bridge).contains("expired"));
    assert!(bridge.state.mappings[&key].launch_draft.is_none());
    assert!(bridge.state.mappings[&key].prompt_queue.is_empty());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    server.abort();
}

#[tokio::test]
async fn short_commands_select_options_but_only_explicit_text_confirms_a_model_switch() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    let (server, _) = host(&config, Some(agent)).await;
    bridge.inbound(&event("new", "/new")).await.unwrap();
    bridge
        .inbound(&event("workspace", "/new workspace 1"))
        .await
        .unwrap();
    bridge
        .inbound(&event("folder", "/new folder another-project"))
        .await
        .unwrap();
    let index = bridge.state.mappings[&key]
        .launch_draft
        .as_ref()
        .unwrap()
        .agents
        .iter()
        .position(|a| a == "opencode")
        .unwrap()
        + 1;
    bridge
        .inbound(&event("agent", &format!("/agent {index}")))
        .await
        .unwrap();
    bridge
        .inbound(&event("provider", "/model 2"))
        .await
        .unwrap();
    bridge.inbound(&event("model", "/model 2")).await.unwrap();
    assert!(bridge.state.mappings[&key].launch.is_some());
    bridge
        .inbound(&event("cancel", "/new cancel"))
        .await
        .unwrap();
    bridge.inbound(&event("switch", "/model")).await.unwrap();
    bridge
        .inbound(&event("provider-switch", "1"))
        .await
        .unwrap();
    bridge.inbound(&event("choose", "2")).await.unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    assert_eq!(
        draft.step,
        herdr_white_noise_bridge::launch_flow::Step::ModelConfirm
    );
    assert!(!last(&bridge).contains(&draft.code));
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    for field in ["text_truncated", "attachments"] {
        let mut msg = event(field, "/model confirm");
        msg["message"][field] = if field == "text_truncated" {
            json!(true)
        } else {
            json!([{"media_type":"image/png"}])
        };
        bridge.inbound(&msg).await.unwrap();
        assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    }
    bridge
        .inbound(&event("confirm", "/model confirm"))
        .await
        .unwrap();
    assert_eq!(
        api_calls(&config).iter().filter(|c| c[1] == "post").count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn legacy_proposals_invalidate_older_guided_folder_and_model_codes() {
    for model_menu in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let agent = source(&config, "opencode");
        let (mut bridge, key) = bridge(&config, Some(agent.clone()));
        let (server, calls) = host(&config, Some(agent)).await;
        bridge
            .inbound(&event("begin", if model_menu { "/model" } else { "/new" }))
            .await
            .unwrap();
        if model_menu {
            let code = model_code(&mut bridge, &key).await;
            bridge
                .inbound(&event("choose", &format!("/model {code} 2")))
                .await
                .unwrap();
        }
        let old = draft_code(&bridge, &key);
        bridge
            .inbound(&event("legacy", "/new newer newer opencode"))
            .await
            .unwrap();
        let launch_code = bridge.state.mappings[&key]
            .launch
            .as_ref()
            .unwrap()
            .code
            .clone();
        let stale = if model_menu {
            format!("/model confirm {old}")
        } else {
            format!("/new folder {old} older")
        };
        bridge.inbound(&event("stale", &stale)).await.unwrap();
        assert!(bridge.state.mappings[&key].launch_draft.is_none());
        assert_eq!(
            bridge.state.mappings[&key].launch.as_ref().unwrap().code,
            launch_code
        );
        assert_eq!(
            bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
            herdr_white_noise_bridge::launch::Phase::Awaiting
        );
        assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| c["method"] == "agent.get" || c["method"] == "workspace.list")
        );
        server.abort();
    }
}

#[tokio::test]
async fn fresh_agent_selection_keeps_a_dotted_project_directory() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let mut agent = source(&config, "opencode");
    agent.cwd = config
        .launch_root
        .join("project.v2")
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir(&agent.cwd).unwrap();
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    let (server, _) = host(&config, Some(agent.clone())).await;
    bridge.inbound(&event("open", "/agent")).await.unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    let n = draft.agents.iter().position(|a| a == "opencode").unwrap() + 1;
    let code = draft.code.clone();
    bridge
        .inbound(&event("agent", &format!("/agent {code} {n}")))
        .await
        .unwrap();
    let code = model_code(&mut bridge, &key).await;
    bridge
        .inbound(&event("model", &format!("/model {code} 2")))
        .await
        .unwrap();
    let request = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .expect("valid dotted directory must reach confirmation");
    assert_eq!(request.directory, Path::new(&agent.cwd));
    assert_eq!(request.label, "project-v2");
    server.abort();
}
fn draft_code(bridge: &Bridge, key: &str) -> String {
    bridge.state.mappings[key]
        .launch_draft
        .as_ref()
        .unwrap()
        .code
        .clone()
}

fn api(config: &BridgeConfig) {
    std::fs::create_dir_all(config.launch_root.join("existing")).unwrap();
    let rows:Vec<_>=(1..=14).map(|i|json!({"id":format!("model-{i}"),"providerID":"test","name":format!("Model {i}"),"enabled":true,"status":"active","variants":[]})).collect();
    let state = json!({"models":rows,"sessions":{"ses_existing":{"id":"ses_existing","location":{"directory":config.launch_root.join("existing")},"model":{"id":"model-1","providerID":"test"}}},"mode":"ok"});
    std::fs::write(
        config.opencode_bin.with_extension("json"),
        state.to_string(),
    )
    .unwrap();
    std::fs::write(&config.opencode_bin,r#"#!/usr/bin/python3
import sys,json,pathlib
p=pathlib.Path(__file__); store=p.with_suffix('.json'); state=json.loads(store.read_text()); args=sys.argv[1:]
with p.with_suffix('.calls').open('a') as f:f.write(json.dumps(args)+'\n')
assert args[0]=='api'
method,path=args[1:3]; body=json.loads(args[args.index('--data')+1]) if '--data' in args else None
if path.startswith('/api/model?'): print(json.dumps({'data':state['models']}));sys.exit()
if method=='post' and path=='/api/session':
 if state['mode']=='fail-create': sys.exit(1)
 if 'canonical_variant' in state: body['model']['variant']=state['canonical_variant']
 state['sessions'][body['id']]=body;store.write_text(json.dumps(state));print(json.dumps({'data':body}));sys.exit()
session=path.split('/')[3]
if method=='post' and path.endswith('/model'):
 if 'canonical_variant' in state: body['model']['variant']=state['canonical_variant']
 state['sessions'][session]['model']=body['model'];store.write_text(json.dumps(state))
 if state['mode']=='lost-switch':sys.exit(1)
 sys.exit()
if method=='get':print(json.dumps({'data':state['sessions'][session]}));sys.exit()
raise Exception('Unexpected API')
"#).unwrap();
    std::fs::set_permissions(&config.opencode_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn api_state(config: &BridgeConfig) -> Value {
    serde_json::from_slice(&std::fs::read(config.opencode_bin.with_extension("json")).unwrap())
        .unwrap()
}
fn change_api_state(config: &BridgeConfig, f: impl FnOnce(&mut Value)) {
    let mut state = api_state(config);
    f(&mut state);
    std::fs::write(
        config.opencode_bin.with_extension("json"),
        state.to_string(),
    )
    .unwrap();
}
fn api_calls(config: &BridgeConfig) -> Vec<Value> {
    std::fs::read_to_string(config.opencode_bin.with_extension("calls"))
        .unwrap_or_default()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
async fn host(
    config: &BridgeConfig,
    source: Option<Agent>,
) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    host_with_workspaces(
        config,
        source,
        Arc::new(Mutex::new(vec![
            json!({"workspace_id":"w1","label":"Main"}),
            json!({"workspace_id":"w2","label":"Other"}),
        ])),
    )
    .await
}

async fn host_with_workspaces(
    config: &BridgeConfig,
    source: Option<Agent>,
    workspaces: Arc<Mutex<Vec<Value>>>,
) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    let listener = UnixListener::bind(&config.herdr_socket).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let captured = calls.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let request = read_line(&mut stream).await.unwrap();
            captured.lock().unwrap().push(request.clone());
            if request["method"] == "agent.start" {
                let flags = workspaces.lock().unwrap().clone();
                let busy = flags
                    .iter()
                    .find_map(|w| w["agent_start_busy"].as_u64())
                    .unwrap_or(0);
                let starts = captured
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c["method"] == "agent.start")
                    .count() as u64;
                let code = flags
                    .iter()
                    .find_map(|w| w["agent_start_error"].as_str().map(str::to_owned))
                    .or_else(|| (starts <= busy).then(|| "agent_pane_busy".to_owned()));
                if let Some(code) = code {
                    let error =
                        json!({"id":request["id"],"error":{"code":code,"message":"refused"}});
                    stream
                        .get_mut()
                        .write_all(format!("{error}\n").as_bytes())
                        .await
                        .unwrap();
                    continue;
                }
            }
            let result = match request["method"].as_str().unwrap() {
                "workspace.list" => {
                    json!({"type":"workspace_list","workspaces":workspaces.lock().unwrap().clone()})
                }
                "workspace.get" => {
                    json!({"type":"workspace_info","workspace":workspaces.lock().unwrap().iter().find(|w|w["workspace_id"]==request["params"]["workspace_id"]).cloned()})
                }
                "session.snapshot" => {
                    let agents = workspaces
                        .lock()
                        .unwrap()
                        .iter()
                        .find_map(|w| w["snapshot_agents"].as_array().cloned())
                        .unwrap_or_else(|| source.clone().into_iter().map(|a| json!(a)).collect());
                    json!({"snapshot":{"agents":agents}})
                }
                "agent.get" => {
                    let agents = workspaces
                        .lock()
                        .unwrap()
                        .iter()
                        .find_map(|w| w["snapshot_agents"].as_array().cloned());
                    let agent = agents
                        .map(|agents| {
                            agents
                                .into_iter()
                                .find(|a| a["pane_id"] == request["params"]["target"])
                        })
                        .unwrap_or_else(|| source.clone().map(|a| json!(a)));
                    json!({"agent":agent})
                }
                "workspace.create" => {
                    json!({"type":"workspace_created","workspace":{"workspace_id":"w99"},"root_pane":{"pane_id":"w99:p1","terminal_id":"t99"}})
                }
                "tab.create" => {
                    if workspaces
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|w| w["simulate_lost_tab_ack"] == true)
                    {
                        break;
                    }
                    json!({"type":"tab_created","tab":{"tab_id":"w1:t99","workspace_id":"w1"},"root_pane":{"pane_id":"w1:p99","terminal_id":"t99"}})
                }
                "agent.start" => {
                    let fresh = request["params"]["pane_id"] == "w1:p99";
                    json!({"type":"agent_started","agent":{"name":request["params"]["name"],"pane_id":request["params"]["pane_id"],"terminal_id":"t99","workspace_id":if fresh {"w1"}else{"w99"}}})
                }
                m => panic!("Unexpected terminal mutation: {m}"),
            };
            stream
                .get_mut()
                .write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes())
                .await
                .unwrap();
        }
    });
    (task, calls)
}

async fn new_model_preview(bridge: &mut Bridge, key: &str) {
    bridge.inbound(&event("start", "/new")).await.unwrap();
    let code = draft_code(bridge, key);
    bridge
        .inbound(&event("workspace", &format!("/new workspace {code} new")))
        .await
        .unwrap();
    let code = draft_code(bridge, key);
    bridge
        .inbound(&event("folder", &format!("/new folder {code} my-idea")))
        .await
        .unwrap();
    let draft = bridge.state.mappings[key].launch_draft.as_ref().unwrap();
    let n = draft.agents.iter().position(|a| a == "opencode").unwrap() + 1;
    let code = draft.code.clone();
    bridge
        .inbound(&event("agent", &format!("/agent {code} {n}")))
        .await
        .unwrap();
    let code = model_code(bridge, key).await;
    bridge
        .inbound(&event("model", &format!("/model {code} 2")))
        .await
        .unwrap();
    assert!(last(bridge).contains("Model 2"));
    assert!(last(bridge).contains("Folder will be created"));
}

#[tokio::test]
async fn new_project_can_choose_an_existing_workspace_without_touching_its_old_sessions() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let source = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, Some(source.clone()));
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "old-waiting".into(),
            group: "bb".repeat(16),
            text: "Keep this old task".into(),
            media: None,
        });
    bridge.state.mappings.get_mut(&key).unwrap().pending_prompt = true;
    bridge.save().unwrap();
    herdr_white_noise_bridge::forwarding::set(&config.state_path, false).unwrap();
    let old = bridge.state.mappings[&key].clone();
    let (server, calls) = host(&config, Some(source)).await;
    bridge.inbound(&event("begin", "/new")).await.unwrap();
    assert!(last(&bridge).contains("Main"));
    assert!(last(&bridge).contains("Create new workspace"));
    let code = draft_code(&bridge, &key);
    bridge
        .inbound(&event("workspace", &format!("/new workspace {code} 1")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Folder name"));
    let code = draft_code(&bridge, &key);
    bridge
        .inbound(&event(
            "folder",
            &format!("/new folder {code} side-project"),
        ))
        .await
        .unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    let n = draft.agents.iter().position(|a| a == "opencode").unwrap() + 1;
    let code = draft.code.clone();
    bridge
        .inbound(&event("agent", &format!("/agent {code} {n}")))
        .await
        .unwrap();
    let code = model_code(&mut bridge, &key).await;
    bridge
        .inbound(&event("model", &format!("/model {code} 2")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Main (w1)"));
    assert!(last(&bridge).contains("existing workspace"));
    assert!(!config.launch_root.join("side-project").exists());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    let code = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone();
    let confirm = event("confirm", &format!("/new confirm {code}"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&confirm).await.unwrap();
    assert!(last(&bridge).contains("workspace Main (w1)"));
    bridge
        .inbound(&event("duplicate", &format!("/new confirm {code}")))
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    let tabs: Vec<_> = calls
        .iter()
        .filter(|c| c["method"] == "tab.create")
        .collect();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["params"]["workspace_id"], "w1");
    assert_eq!(tabs[0]["params"]["label"], "side-project");
    assert_eq!(
        tabs[0]["params"]["cwd"],
        config.launch_root.join("side-project").to_str().unwrap()
    );
    assert_eq!(tabs[0]["params"]["focus"], false);
    assert!(!calls.iter().any(|c| c["method"] == "workspace.create"));
    assert_eq!(bridge.state.mappings[&key].agent_record, old.agent_record);
    assert_eq!(bridge.state.mappings[&key].group_id_hex, old.group_id_hex);
    assert_eq!(bridge.state.mappings[&key].prompt_queue, old.prompt_queue);
    assert_eq!(
        bridge
            .state
            .pending_chat_agents
            .get("t99")
            .map(String::as_str),
        Some("opencode")
    );
    assert!(bridge.state.mappings[&key].pending_prompt);
    assert!(!bridge.state.forwarding_enabled);
    server.abort();
}

async fn existing_workspace_preview(bridge: &mut Bridge, key: &str, index: usize) -> String {
    bridge.inbound(&event("begin", "/new")).await.unwrap();
    let code = draft_code(bridge, key);
    bridge
        .inbound(&event(
            "workspace",
            &format!("/new workspace {code} {index}"),
        ))
        .await
        .unwrap();
    let code = draft_code(bridge, key);
    bridge
        .inbound(&event(
            "folder",
            &format!("/new folder {code} side-project"),
        ))
        .await
        .unwrap();
    let draft = bridge.state.mappings[key].launch_draft.as_ref().unwrap();
    let n = draft.agents.iter().position(|a| a == "opencode").unwrap() + 1;
    let code = draft.code.clone();
    bridge
        .inbound(&event("agent", &format!("/agent {code} {n}")))
        .await
        .unwrap();
    let code = model_code(bridge, key).await;
    bridge
        .inbound(&event("model", &format!("/model {code} 1")))
        .await
        .unwrap();
    bridge.state.mappings[key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone()
}

#[tokio::test]
async fn a_missing_selected_workspace_cannot_create_a_folder_or_fall_back_to_a_new_workspace() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let workspaces = Arc::new(Mutex::new(vec![
        json!({"workspace_id":"w1","label":"Main"}),
    ]));
    let (server, calls) = host_with_workspaces(&config, None, workspaces.clone()).await;
    let code = existing_workspace_preview(&mut bridge, &key, 1).await;
    workspaces.lock().unwrap().clear();
    bridge
        .inbound(&event("confirm", &format!("/new confirm {code}")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("selected workspace is unavailable"));
    assert!(!config.launch_root.join("side-project").exists());
    assert!(calls.lock().unwrap().iter().all(|c| matches!(
        c["method"].as_str(),
        Some("workspace.list" | "workspace.get")
    )));
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    server.abort();
}

#[tokio::test]
async fn a_renamed_workspace_requires_a_new_confirmation_code() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let workspaces = Arc::new(Mutex::new(vec![
        json!({"workspace_id":"w1","label":"Main"}),
    ]));
    let (server, calls) = host_with_workspaces(&config, None, workspaces.clone()).await;
    let old = existing_workspace_preview(&mut bridge, &key, 1).await;
    workspaces.lock().unwrap()[0]["label"] = json!("Renamed");
    bridge
        .inbound(&event("confirm-old", &format!("/new confirm {old}")))
        .await
        .unwrap();
    let new = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone();
    assert_ne!(old, new);
    assert!(last(&bridge).contains("Renamed (w1)"));
    assert!(!config.launch_root.join("side-project").exists());
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    bridge
        .inbound(&event("stale", &format!("/new confirm {old}")))
        .await
        .unwrap();
    assert!(!config.launch_root.join("side-project").exists());
    bridge
        .inbound(&event("updated", &format!("/new confirm {new}")))
        .await
        .unwrap();
    assert!(config.launch_root.join("side-project").is_dir());
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "tab.create")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn workspace_pages_and_stale_choices_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let rows: Vec<_> = (1..=12)
        .map(|i| json!({"workspace_id":format!("w{i}"),"label":"Same label"}))
        .collect();
    let (server, calls) = host_with_workspaces(&config, None, Arc::new(Mutex::new(rows))).await;
    bridge.inbound(&event("begin", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    assert!(last(&bridge).contains("page 1 of 2"));
    assert!(last(&bridge).contains("13. Create new workspace\n\n**0. Cancel**"));
    assert!(last(&bridge).contains("Next page: next"));
    bridge
        .inbound(&event("page", &format!("/new workspace {code} next")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("11. Same label (w11)"));
    assert!(
        last(&bridge).find("12. Same label (w12)").unwrap()
            < last(&bridge).find("13. Create new workspace").unwrap()
    );
    assert!(!last(&bridge).contains("1. Same label (w1)"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    let selection = event("select", &format!("/new workspace {code} 11"));
    bridge.inbound(&selection).await.unwrap();
    let folder_code = draft_code(&bridge, &key);
    assert_ne!(code, folder_code);
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .target_workspace
            .as_ref()
            .unwrap()
            .workspace_id,
        "w11"
    );
    bridge.inbound(&selection).await.unwrap();
    bridge
        .inbound(&event("stale", &format!("/new workspace {code} 1")))
        .await
        .unwrap();
    assert_eq!(draft_code(&bridge, &key), folder_code);
    assert_eq!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .target_workspace
            .as_ref()
            .unwrap()
            .workspace_id,
        "w11"
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    assert!(api_calls(&config).is_empty());
    server.abort();
}

#[tokio::test]
async fn an_empty_workspace_list_still_allows_an_explicit_new_workspace() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, _) = host_with_workspaces(&config, None, Arc::new(Mutex::new(vec![]))).await;
    bridge.inbound(&event("begin", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    assert!(!last(&bridge).contains("unavailable"));
    bridge
        .inbound(&event("invalid", &format!("/new workspace {code} 2")))
        .await
        .unwrap();
    assert_eq!(draft_code(&bridge, &key), code);
    bridge
        .inbound(&event("new", &format!("/new workspace {code} new")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Folder name"));
    assert!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .target_workspace
            .is_none()
    );
    server.abort();
}

#[tokio::test]
async fn uncertain_tab_creation_in_a_selected_workspace_is_never_repeated() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let workspaces = Arc::new(Mutex::new(vec![
        json!({"workspace_id":"w1","label":"Main"}),
    ]));
    let (server, calls) = host_with_workspaces(&config, None, workspaces.clone()).await;
    let code = existing_workspace_preview(&mut bridge, &key, 1).await;
    workspaces.lock().unwrap()[0]["simulate_lost_tab_ack"] = json!(true);
    bridge
        .inbound(&event("confirm", &format!("/new confirm {code}")))
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Uncertain
    );
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .inbound(&event("retry", &format!("/new confirm {code}")))
        .await
        .unwrap();
    bridge.inbound(&event("another", "/new")).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "tab.create")
            .count(),
        1
    );
    assert!(!calls.lock().unwrap().iter().any(|c| matches!(
        c["method"].as_str(),
        Some("workspace.create" | "agent.start")
    )));
    assert_eq!(
        api_calls(&config).iter().filter(|c| c[1] == "post").count(),
        1
    );
}

#[tokio::test]
async fn a_tab_acknowledged_in_the_wrong_workspace_cannot_start_an_agent() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    let code = existing_workspace_preview(&mut bridge, &key, 2).await;
    bridge
        .inbound(&event("confirm", &format!("/new confirm {code}")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("response was invalid"));
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Uncertain
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["method"] == "agent.start")
    );
    server.abort();
}

#[tokio::test]
async fn saved_folder_drafts_from_before_workspace_selection_keep_the_new_workspace_default() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, _) = host(&config, None).await;
    bridge.inbound(&event("begin", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    let mut draft =
        serde_json::to_value(bridge.state.mappings[&key].launch_draft.as_ref().unwrap()).unwrap();
    draft["step"] = json!("Folder");
    for field in ["workspaces", "workspace_list_available", "target_workspace"] {
        draft.as_object_mut().unwrap().remove(field);
    }
    bridge.state.mappings.get_mut(&key).unwrap().launch_draft =
        Some(serde_json::from_value(draft).unwrap());
    bridge.save().unwrap();
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .inbound(&event("folder", &format!("/new folder {code} existing")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Choose agent:"));
    assert!(
        bridge.state.mappings[&key]
            .launch_draft
            .as_ref()
            .unwrap()
            .target_workspace
            .is_none()
    );
    server.abort();
}

#[tokio::test]
async fn the_legacy_shortcut_can_still_name_a_project_workspace() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    bridge
        .inbound(&event("legacy", "/new workspace named-folder opencode"))
        .await
        .unwrap();
    let request = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .expect("Legacy project name must remain available");
    assert_eq!(request.label, "workspace");
    assert!(request.target_workspace.is_none());
    assert!(!config.launch_root.join("named-folder").exists());
}

#[tokio::test]
async fn guided_new_creates_a_model_selected_native_session_only_after_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    new_model_preview(&mut bridge, &key).await;
    assert!(!config.launch_root.join("my-idea").exists());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["method"] == "workspace.list")
    );
    assert_eq!(
        api_calls(&config).iter().filter(|c| c[1] == "post").count(),
        0
    );
    let code = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone();
    let confirm = event("confirm", &format!("/new confirm {code}"));
    bridge.inbound(&confirm).await.unwrap();
    assert!(config.launch_root.join("my-idea").is_dir());
    let native = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .native_session
        .as_ref()
        .expect("native session bound before launch");
    assert_eq!(
        api_state(&config)["sessions"][native]["model"],
        json!({"id":"model-2","providerID":"test"})
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c["method"] == "agent.start")
            .unwrap()["params"]["args"],
        json!(["--session", native])
    );
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&confirm).await.unwrap();
    bridge
        .inbound(&event("again", &format!("/new confirm {code}")))
        .await
        .unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] != "workspace.list")
            .count(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn new_project_accepts_opencodes_canonical_default_variant_without_duplicate_sessions() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    change_api_state(&config, |s| s["canonical_variant"] = json!("default"));
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    existing_workspace_preview(&mut bridge, &key, 1).await;
    let confirmation = event("confirm-project", "side-project");
    bridge.inbound(&confirmation).await.unwrap();
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Started
    );
    let native = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .native_session
        .clone()
        .unwrap();
    assert_eq!(
        api_state(&config)["sessions"][&native]["model"]["variant"],
        "default"
    );
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge.inbound(&confirmation).await.unwrap();
    bridge
        .inbound(&event("confirm-again", "/new confirm"))
        .await
        .unwrap();
    assert_eq!(
        api_calls(&config)
            .iter()
            .filter(|c| c[1] == "post" && c[2] == "/api/session")
            .count(),
        1
    );
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().filter(|c| c["method"] == "tab.create").count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["method"] == "agent.start")
            .count(),
        1
    );
    assert!(!calls.iter().any(|c| c["method"] == "workspace.create"));
    server.abort();
}

#[tokio::test]
async fn new_project_does_not_accept_an_unrequested_nondefault_variant() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    change_api_state(&config, |s| s["canonical_variant"] = json!("high"));
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host(&config, None).await;
    existing_workspace_preview(&mut bridge, &key, 1).await;
    bridge
        .inbound(&event("confirm-project", "side-project"))
        .await
        .unwrap();
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Uncertain
    );
    assert!(calls.lock().unwrap().iter().all(|c| matches!(
        c["method"].as_str(),
        Some("workspace.list" | "workspace.get")
    )));
    server.abort();
}

#[tokio::test]
async fn agent_selection_creates_a_separate_tab_without_changing_the_old_chat() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let mut agent = source(&config, "opencode");
    agent.tab_label = Some("review".into());
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .prompt_queue
        .push(herdr_white_noise_bridge::state::QueuedPrompt {
            id: "keep-waiting".into(),
            group: "bb".repeat(16),
            text: "Preserve this queued prompt".into(),
            media: None,
        });
    bridge.state.mappings.get_mut(&key).unwrap().pending_prompt = true;
    bridge.save().unwrap();
    let old = bridge.state.mappings[&key].clone();
    let (server, calls) = host(&config, Some(agent)).await;
    bridge.inbound(&event("open", "/agent")).await.unwrap();
    assert!(last(&bridge).contains("separate chat"));
    assert!(last(&bridge).starts_with("Choose an installed agent:\n"));
    assert!(last(&bridge).contains("Reply with a listed number."));
    assert!(last(&bridge).ends_with("Menus expire after 10 minutes."));
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    let n = draft.agents.iter().position(|a| a == "opencode").unwrap() + 1;
    let code = draft.code.clone();
    bridge
        .inbound(&event("agent", &format!("/agent {code} {n}")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("models from a provider."));
    assert!(last(&bridge).contains("Reply with a listed number."));
    assert!(last(&bridge).ends_with("Menus expire after 10 minutes."));
    let code = model_code(&mut bridge, &key).await;
    bridge
        .inbound(&event("choose", &format!("/model {code} 2")))
        .await
        .unwrap();
    let code = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone();
    bridge
        .inbound(&event("confirm", &format!("/new confirm {code}")))
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    let tab = calls
        .iter()
        .find(|r| r["method"] == "tab.create")
        .expect("new tab in same workspace");
    assert_eq!(tab["params"]["workspace_id"], "w1");
    assert_eq!(tab["params"]["label"], "review");
    assert_eq!(tab["params"]["focus"], false);
    assert!(!calls.iter().any(|r| r["method"] == "workspace.create"));
    assert_eq!(bridge.state.mappings[&key].group_id_hex, old.group_id_hex);
    assert_eq!(bridge.state.mappings[&key].agent_record, old.agent_record);
    assert_eq!(bridge.state.mappings[&key].prompt_queue, old.prompt_queue);
    assert!(bridge.state.mappings[&key].pending_prompt);
    assert_eq!(
        bridge
            .state
            .pending_chat_agents
            .get("t99")
            .map(String::as_str),
        Some("opencode")
    );
    server.abort();
}

#[tokio::test]
async fn existing_opencode_model_switch_is_confirmed_in_place_and_never_repeated() {
    for canonical in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        if canonical {
            change_api_state(&config, |s| s["canonical_variant"] = json!("default"));
        }
        let agent = source(&config, "opencode");
        let (mut bridge, key) = bridge(&config, Some(agent.clone()));
        let (server, calls) = host(&config, Some(agent)).await;
        bridge.inbound(&event("open", "/model")).await.unwrap();
        assert!(last(&bridge).starts_with("Choose a model for OpenCode:\n"));
        assert!(last(&bridge).contains("models from a provider."));
        assert!(last(&bridge).contains("Reply with a listed number."));
        assert!(last(&bridge).ends_with("Menus expire after 10 minutes."));
        let code = model_code(&mut bridge, &key).await;
        bridge
            .inbound(&event("choose", &format!("/model {code} 2")))
            .await
            .unwrap();
        let code = draft_code(&bridge, &key);
        assert!(last(&bridge).contains("/model confirm"));
        assert_eq!(
            api_state(&config)["sessions"]["ses_existing"]["model"]["id"],
            "model-1"
        );
        let confirmation = event("confirm", &format!("/model confirm {code}"));
        bridge.inbound(&confirmation).await.unwrap();
        assert_eq!(
            api_state(&config)["sessions"]["ses_existing"]["model"]["id"],
            "model-2"
        );
        assert!(last(&bridge).contains("Model changed"));
        drop(bridge);
        let mut bridge = Bridge::open(config.clone()).unwrap();
        bridge.inbound(&confirmation).await.unwrap();
        bridge
            .inbound(&event("again", &format!("/model confirm {code}")))
            .await
            .unwrap();
        assert_eq!(
            api_calls(&config).iter().filter(|c| c[1] == "post").count(),
            1
        );
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| c["method"] == "agent.get")
        );
        server.abort();
    }
}

#[tokio::test]
async fn cancelling_or_restarting_setup_invalidates_an_old_launch_confirmation() {
    for cancel in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let (mut bridge, key) = bridge(&config, None);
        new_model_preview(&mut bridge, &key).await;
        let old = bridge.state.mappings[&key]
            .launch
            .as_ref()
            .unwrap()
            .code
            .clone();
        bridge
            .inbound(&event("reset", if cancel { "/new cancel" } else { "/new" }))
            .await
            .unwrap();
        let (server, calls) = host(&config, None).await;
        bridge
            .inbound(&event("old-confirm", &format!("/new confirm {old}")))
            .await
            .unwrap();
        assert!(
            !config.launch_root.join("my-idea").exists(),
            "Old confirmation must be invalid after cancellation or a new setup"
        );
        assert!(calls.lock().unwrap().is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn model_pages_and_step_codes_survive_restart_without_consuming_ordinary_messages() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, _) = host(&config, None).await;
    bridge.inbound(&event("start", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    bridge
        .inbound(&event("workspace", &format!("/new workspace {code} new")))
        .await
        .unwrap();
    let folder_code = draft_code(&bridge, &key);
    bridge
        .inbound(&event(
            "folder",
            &format!("/new folder {folder_code} existing"),
        ))
        .await
        .unwrap();
    let draft = bridge.state.mappings[&key].launch_draft.as_ref().unwrap();
    let n = draft.agents.iter().position(|a| a == "opencode").unwrap() + 1;
    let agent_code = draft.code.clone();
    bridge
        .inbound(&event("agent", &format!("/agent {agent_code} {n}")))
        .await
        .unwrap();
    // Old on-disk flat menus must keep their page numbers and default-model offset.
    let draft = bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap();
    draft.model_groups.clear();
    draft.visible_models.clear();
    draft.step = herdr_white_noise_bridge::launch_flow::Step::Model;
    bridge.save().unwrap();
    bridge
        .inbound(&event("legacy-show", "/model"))
        .await
        .unwrap();
    let model_code = draft_code(&bridge, &key);
    assert_ne!(agent_code, model_code);
    assert!(last(&bridge).contains("page 1 of 2"));
    bridge
        .inbound(&event("page", &format!("/model {model_code} next")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("page 2 of 2"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .inbound(&event("stale", &format!("/agent {agent_code} 1")))
        .await
        .unwrap();
    assert_eq!(draft_code(&bridge, &key), model_code);
    bridge
        .inbound(&event("choose", &format!("/model {model_code} 12")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("Model 11"));
    assert!(last(&bridge).contains("Folder already exists"));
    assert!(bridge.state.mappings[&key].launch_draft.is_none());
    assert_eq!(
        bridge.state.mappings[&key]
            .launch
            .as_ref()
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .id,
        "model-11"
    );
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    server.abort();
}

#[tokio::test]
async fn unavailable_selected_models_do_not_create_a_folder_or_workspace() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    new_model_preview(&mut bridge, &key).await;
    let code = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone();
    change_api_state(&config, |s| s["models"] = json!([]));
    bridge
        .inbound(&event("confirm", &format!("/new confirm {code}")))
        .await
        .unwrap();
    assert!(!config.launch_root.join("my-idea").exists());
    assert!(last(&bridge).contains("could not be revalidated"));
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
}

#[tokio::test]
async fn an_unconfirmed_native_session_creation_is_never_repeated() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    new_model_preview(&mut bridge, &key).await;
    let code = bridge.state.mappings[&key]
        .launch
        .as_ref()
        .unwrap()
        .code
        .clone();
    change_api_state(&config, |s| s["mode"] = json!("fail-create"));
    bridge
        .inbound(&event("confirm", &format!("/new confirm {code}")))
        .await
        .unwrap();
    assert!(config.launch_root.join("my-idea").is_dir());
    assert!(last(&bridge).contains("session creation was not confirmed"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    bridge
        .inbound(&event("retry", &format!("/new confirm {code}")))
        .await
        .unwrap();
    assert_eq!(
        api_calls(&config).iter().filter(|c| c[1] == "post").count(),
        1
    );
}

#[tokio::test]
async fn model_switch_waits_for_pending_work_and_rechecks_the_live_location() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    let (server, _) = host(&config, Some(agent)).await;
    bridge.inbound(&event("open", "/model")).await.unwrap();
    let code = model_code(&mut bridge, &key).await;
    bridge
        .inbound(&event("choose", &format!("/model {code} 2")))
        .await
        .unwrap();
    let code = draft_code(&bridge, &key);
    bridge.state.mappings.get_mut(&key).unwrap().pending_prompt = true;
    bridge
        .inbound(&event("busy", &format!("/model confirm {code}")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("idle"));
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    bridge.state.mappings.get_mut(&key).unwrap().pending_prompt = false;
    change_api_state(&config, |s| {
        s["sessions"]["ses_existing"]["location"]["directory"] = json!("/another/project")
    });
    bridge
        .inbound(&event("changed", &format!("/model confirm {code}")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("identity or location changed"));
    assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
    server.abort();
}

#[tokio::test]
async fn an_uncertain_model_switch_cannot_be_repeated_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let agent = source(&config, "opencode");
    let (mut bridge, key) = bridge(&config, Some(agent.clone()));
    let (server, _) = host(&config, Some(agent)).await;
    bridge.inbound(&event("open", "/model")).await.unwrap();
    let code = model_code(&mut bridge, &key).await;
    bridge
        .inbound(&event("choose", &format!("/model {code} 2")))
        .await
        .unwrap();
    let code = draft_code(&bridge, &key);
    change_api_state(&config, |s| s["mode"] = json!("lost-switch"));
    bridge
        .inbound(&event("confirm", &format!("/model confirm {code}")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("not confirmed"));
    drop(bridge);
    let mut bridge = Bridge::open(config.clone()).unwrap();
    for (id, text) in [
        ("retry", format!("/model confirm {code}")),
        ("new", "/model".into()),
        ("cancel", "/new cancel".into()),
    ] {
        bridge.inbound(&event(id, &text)).await.unwrap();
    }
    assert_eq!(
        api_calls(&config).iter().filter(|c| c[1] == "post").count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn unsafe_menu_replies_cannot_advance_the_draft() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    bridge.inbound(&event("open", "/new")).await.unwrap();
    let code = draft_code(&bridge, &key);
    for (i, field) in [
        "text_truncated",
        "attachments_truncated",
        "attachments",
        "media",
        "foreign",
    ]
    .iter()
    .enumerate()
    {
        let mut msg = event(&format!("bad-{i}"), &format!("/new workspace {code} new"));
        match *field {
            "foreign" => msg["message"]["sender"]["account_id_hex"] = json!("99".repeat(32)),
            "text_truncated" | "attachments_truncated" => msg["message"][field] = json!(true),
            _ => msg["message"][field] = json!([{"media_type":"image/png"}]),
        }
        bridge.inbound(&msg).await.unwrap();
        assert_eq!(draft_code(&bridge, &key), code);
    }
    bridge
        .state
        .mappings
        .get_mut(&key)
        .unwrap()
        .launch_draft
        .as_mut()
        .unwrap()
        .expires_at = 0;
    bridge
        .inbound(&event("expired", &format!("/new workspace {code} new")))
        .await
        .unwrap();
    assert!(last(&bridge).contains("expired"));
    assert!(!config.launch_root.join("idea").exists());
}

#[tokio::test]
async fn failed_menu_or_model_intent_save_prevents_api_mutations() {
    for model_switch in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let agent = source(&config, "opencode");
        let (mut bridge, key) = bridge(&config, Some(agent.clone()));
        let (server, _) = host(&config, Some(agent)).await;
        bridge
            .inbound(&event("open", if model_switch { "/model" } else { "/new" }))
            .await
            .unwrap();
        if model_switch {
            let code = model_code(&mut bridge, &key).await;
            bridge
                .inbound(&event("choose", &format!("/model {code} 2")))
                .await
                .unwrap();
        }
        let old = bridge.state.mappings[&key].launch_draft.clone();
        let code = draft_code(&bridge, &key);
        std::fs::rename(&config.state_path, root.path().join("saved-state.json")).unwrap();
        std::fs::create_dir(&config.state_path).unwrap();
        let command = if model_switch {
            format!("/model confirm {code}")
        } else {
            format!("/new workspace {code} new")
        };
        assert!(bridge.inbound(&event("unsaved", &command)).await.is_err());
        assert_eq!(bridge.state.mappings[&key].launch_draft, old);
        assert!(api_calls(&config).iter().all(|c| c[1] == "get"));
        assert!(!config.launch_root.join("idea").exists());
        server.abort();
    }
}

#[tokio::test]
async fn confirm_words_start_without_typing_the_workspace_name() {
    for word in ["confirm", "yes", "y"] {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        api(&config);
        let (mut bridge, key) = bridge(&config, None);
        let (server, calls) = host(&config, None).await;
        new_model_preview(&mut bridge, &key).await;
        assert!(last(&bridge).contains("Reply confirm, yes or y (or type: my-idea)"));
        bridge.inbound(&event("confirm-word", word)).await.unwrap();
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c["method"] == "workspace.create")
                .count(),
            1,
            "{word} did not start the launch"
        );
        server.abort();
    }
}

#[tokio::test]
async fn a_busy_fresh_pane_is_retried_until_its_shell_is_ready() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    let (server, calls) = host_with_workspaces(
        &config,
        None,
        Arc::new(Mutex::new(vec![
            json!({"workspace_id":"w1","label":"Main","agent_start_busy":2}),
        ])),
    )
    .await;
    new_model_preview(&mut bridge, &key).await;
    bridge.inbound(&event("confirm", "my-idea")).await.unwrap();
    let starts = |calls: &Arc<Mutex<Vec<Value>>>| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.start")
            .count()
    };
    assert_eq!(starts(&calls), 3);
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Started
    );
    server.abort();
}

#[tokio::test]
async fn an_agent_start_failure_other_than_busy_is_not_retried() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    api(&config);
    let (mut bridge, key) = bridge(&config, None);
    // herdr can return agent_pane_unavailable after it has written the command.
    let (server, calls) = host_with_workspaces(
        &config,
        None,
        Arc::new(Mutex::new(vec![
            json!({"workspace_id":"w1","label":"Main","agent_start_error":"agent_pane_unavailable"}),
        ])),
    )
    .await;
    new_model_preview(&mut bridge, &key).await;
    bridge.inbound(&event("confirm", "my-idea")).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "agent.start")
            .count(),
        1
    );
    assert_eq!(
        bridge.state.mappings[&key].launch.as_ref().unwrap().phase,
        herdr_white_noise_bridge::launch::Phase::Uncertain
    );
    server.abort();
}
