//! Owner-authorized desktop access to the running bridge, not a second runtime.
use crate::{
    BridgeConfig,
    bridge::Bridge,
    herdr, output,
    state::QueuedPrompt,
    wire::{MAX_FRAME, Socket, read_line},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    time::timeout,
};

static READ_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
static WATCH_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(16)));

/// Invalidate desktops only when their session/queue projection changes.
pub fn publish(bridge: &Bridge) {
    if bridge.porter_updates.receiver_count() == 0 {
        return;
    }
    let (metadata, forwarding) = snapshot_metadata(bridge);
    let value = json!({"agents":bridge.last_snapshot, "metadata":metadata,
        "forwarding":forwarding, "available":bridge.herdr_available});
    bridge.porter_updates.send_if_modified(|current| {
        if *current == value {
            return false;
        }
        *current = value;
        true
    });
}

async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() < MAX_FRAME, "Oversized desktop frame");
    bytes.push(b'\n');
    timeout(Duration::from_secs(5), async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await??;
    Ok(())
}

/// One SSH command, with bounded local request channels and serialized stdout.
pub async fn forward_stream(state: &Path) -> Result<()> {
    let mut watch = timeout(
        Duration::from_secs(10),
        UnixStream::connect(socket_path(state)),
    )
    .await??;
    write_frame(&mut watch, &json!({"version":1,"method":"watch"})).await?;
    let mut watch = BufReader::new(watch);
    let ready = timeout(Duration::from_secs(10), read_line(&mut watch)).await??;
    ensure!(
        ready["version"] == 1 && ready["event"] == "ready",
        "Desktop subscription rejected"
    );
    let output = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    write_frame(&mut *output.lock().await, &ready).await?;
    let events = async {
        loop {
            let event = read_line(&mut watch).await?;
            write_frame(&mut *output.lock().await, &event).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let requests = async {
        let slots = Arc::new(tokio::sync::Semaphore::new(8));
        let mut input = BufReader::new(tokio::io::stdin());
        loop {
            // read_line treats EOF as an invalid frame; graceful EOF is checked first.
            use tokio::io::AsyncBufReadExt;
            if input.fill_buf().await?.is_empty() {
                break;
            }
            let request = read_line(&mut input).await?;
            ensure!(
                request["version"] == 1
                    && request["id"].is_string()
                    && request["request_id"].is_string(),
                "Invalid desktop request"
            );
            let permit = slots.clone().acquire_owned().await?;
            let socket = Socket {
                path: socket_path(state),
            };
            let output = output.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let mut response = match socket.exchange(request.clone(), false).await {
                    Ok(value) => value,
                    Err(_) => json!({"version":1,"request_id":request["request_id"],
                        "error":{"code":"porter_request_rejected"}}),
                };
                // Socket.exchange uses its own local correlation ID; preserve the SSH ID.
                response["id"] = request["id"].clone();
                let _ = write_frame(&mut *output.lock().await, &response).await;
            });
        }
        let _drained = timeout(Duration::from_secs(110), slots.acquire_many(8)).await??;
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {
        result = events => result,
        result = requests => result,
    }
}

pub fn socket_path(state: &Path) -> PathBuf {
    state.with_extension("porter").join("control.sock")
}

/// Call only while holding the bridge's existing exclusive state lock.
pub fn listen(state: &Path) -> Result<UnixListener> {
    let path = socket_path(state);
    let parent = path.parent().expect("socket has parent");
    match std::fs::DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(parent)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o777 == 0o700,
        "Porter requires a private directory"
    );
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket(),
                "Refusing to replace a non-socket Porter path"
            );
            std::fs::remove_file(&path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn forward(state: &Path) -> Result<()> {
    let request = timeout(
        Duration::from_secs(5),
        read_line(&mut BufReader::new(tokio::io::stdin())),
    )
    .await??;
    let response = Socket {
        path: socket_path(state),
    }
    .exchange(request, false)
    .await?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub async fn accept(listener: &Option<UnixListener>) -> std::io::Result<UnixStream> {
    match listener {
        Some(listener) => listener.accept().await.map(|(stream, _)| stream),
        None => std::future::pending().await,
    }
}

pub async fn serve(bridge: &mut Bridge, stream: UnixStream) -> Result<()> {
    let mut stream = BufReader::new(stream);
    let request = timeout(Duration::from_secs(3), read_line(&mut stream)).await??;
    if request["method"] == "watch" {
        ensure!(request["version"] == 1, "Unsupported desktop version");
        let permit = WATCH_SLOTS.clone().try_acquire_owned()?;
        let mut updates = bridge.porter_updates.subscribe();
        publish(bridge);
        updates.borrow_and_update();
        tokio::spawn(async move {
            let _permit = permit;
            let result: Result<()> = async {
                write_frame(stream.get_mut(), &json!({"version":1,"event":"ready"})).await?;
                let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                heartbeat.tick().await;
                loop {
                    let event = tokio::select! {
                        result = updates.changed() => {
                            result?;
                            "changed"
                        },
                        _ = heartbeat.tick() => "heartbeat",
                    };
                    write_frame(stream.get_mut(), &json!({"version":1,"event":event})).await?;
                }
            }
            .await;
            let _ = result;
        });
        return Ok(());
    }
    if request["method"] == "snapshot" {
        let permit = READ_SLOTS.clone().try_acquire_owned()?;
        let config = bridge.config.clone();
        let (metadata, forwarding) = snapshot_metadata(bridge);
        tokio::spawn(async move {
            let _permit = permit;
            let result = snapshot(&config, &request, metadata, forwarding).await;
            let _ = reply(&mut stream, &request, result).await;
        });
        return Ok(());
    }
    if request["method"] == "history" {
        let permit = READ_SLOTS.clone().try_acquire_owned()?;
        let config = bridge.config.clone();
        let reader = bridge.native_reader();
        // Slow native transcript I/O must not hold the phone/queue event loop.
        tokio::spawn(async move {
            let _permit = permit;
            let result = history(&config, &request, reader).await;
            let _ = reply(&mut stream, &request, result).await;
        });
        return Ok(());
    }
    let result = handle(bridge, request.clone()).await;
    reply(&mut stream, &request, result).await
}

async fn reply(
    stream: &mut BufReader<UnixStream>,
    request: &Value,
    result: Result<Value>,
) -> Result<()> {
    // Never return native reader errors, paths, or transcript content in errors.
    let mut response = match result {
        Ok(result) => {
            json!({"version":1,"id":request["id"],"request_id":request["request_id"],"result":result})
        }
        Err(_) => {
            json!({"version":1,"id":request["id"],"request_id":request["request_id"],"error":{"code":"porter_request_rejected"}})
        }
    };
    let mut bytes = serde_json::to_vec(&response)?;
    if bytes.len() >= MAX_FRAME {
        response =
            json!({"version":1,"id":request["id"],"error":{"code":"porter_response_too_large"}});
        bytes = serde_json::to_vec(&response)?;
    }
    bytes.push(b'\n');
    timeout(Duration::from_secs(5), stream.get_mut().write_all(&bytes)).await??;
    Ok(())
}

pub async fn handle(bridge: &mut Bridge, request: Value) -> Result<Value> {
    ensure!(request["version"] == 1, "Unsupported Porter version");
    let method = request["method"].as_str().unwrap_or("");
    ensure!(
        matches!(method, "snapshot" | "history" | "enqueue" | "receipt"),
        "Unsupported Porter method"
    );
    if method != "snapshot" {
        ensure!(
            request["bridge_account"] == bridge.config.account_id_hex,
            "Plugin account changed"
        );
    }
    if method == "history" {
        return history(&bridge.config, &request, bridge.native_reader()).await;
    }
    if method == "snapshot" {
        let (metadata, forwarding) = snapshot_metadata(bridge);
        return snapshot(&bridge.config, &request, metadata, forwarding).await;
    }
    // Mutations still validate against live herdr before using the durable queue.
    let agents = herdr::snapshot(&bridge.herdr).await?;
    let key = request["session_key"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Session required"))?;
    let agent = agents
        .iter()
        .find(|agent| agent.key() == key && request["terminal_id"] == agent.terminal_id)
        .ok_or_else(|| anyhow::anyhow!("Session identity changed"))?;
    ensure!(
        agent
            .agent_session
            .as_ref()
            .is_some_and(|session| output::supported(&session.agent)),
        "Unsupported session reader"
    );
    let id = request["request_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Request ID required"))?;
    let id = format!("porter:{}", uuid::Uuid::parse_str(id)?);
    if method == "receipt" {
        let mapping = bridge
            .state
            .mappings
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("Session not mapped"))?;
        return Ok(json!({"accepted":mapping.seen_messages.contains(&id)}));
    }
    let text = request["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Text required"))?;
    ensure!(
        !text.trim().is_empty() && text.len() <= 30000,
        "Invalid prompt length"
    );
    // The dispatcher interprets these as phone controls before popping the
    // queue head. They must never enter the desktop's text-only FIFO.
    ensure!(
        !crate::launch_flow::is_command(text)
            && !matches!(
                text.trim().to_ascii_lowercase().as_str(),
                "/clear-queue" | "/queue" | "/help" | "/sync" | "/status" | "unmute" | "/unmute"
            ),
        "Phone control commands are not desktop prompts"
    );
    let before = bridge.state.clone();
    let mapping = bridge
        .state
        .mappings
        .get_mut(key)
        .filter(|m| m.active)
        .ok_or_else(|| anyhow::anyhow!("Session not mapped yet"))?;
    if mapping.seen_messages.contains(&id) {
        return Ok(json!({"status":"already_accepted"}));
    }
    ensure!(mapping.prompt_queue.len() < 16, "Queue limit reached");
    let group = mapping
        .group_id_hex
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Group not mapped yet"))?;
    mapping.prompt_queue.push(QueuedPrompt {
        id: id.clone(),
        group,
        text: text.to_owned(),
        media: None,
    });
    mapping.seen_messages.push(id);
    if !agent.settled() {
        mapping.pending_prompt = true;
    }
    if let Err(error) = bridge.save() {
        bridge.state = before;
        return Err(error);
    }
    Ok(json!({"status":"queued"}))
}

fn snapshot_metadata(bridge: &Bridge) -> (serde_json::Map<String, Value>, bool) {
    let metadata = bridge
        .state
        .mappings
        .iter()
        .map(|(key, m)| {
            (
                key.clone(),
                json!({"mapped":m.active && m.group_id_hex.is_some(),
            "queued":m.prompt_queue.len(), "pending":m.pending_prompt,
            "last_received_at":m.last_received_at, "reply_revision":m.final_ids.len(),
            "uncertain":m.queue_flight.as_ref().is_some_and(|f| !f.confirmed)}),
            )
        })
        .collect();
    (
        metadata,
        bridge.state.forwarding_enabled && !bridge.state.forwarding_resume_pending,
    )
}

async fn snapshot(
    config: &BridgeConfig,
    request: &Value,
    metadata: serde_json::Map<String, Value>,
    forwarding: bool,
) -> Result<Value> {
    ensure!(request["version"] == 1, "Unsupported Porter version");
    // Live herdr reads run outside the worker loop; only small queue metadata is copied.
    let agents = herdr::snapshot(&Socket {
        path: config.herdr_socket.clone(),
    })
    .await?;
    let control = crate::forwarding::load(&config.state_path)?;
    let sessions: Vec<_> = agents
        .iter()
        .map(|agent| {
            let key = agent.key();
            let mut value = metadata.get(&key).cloned().unwrap_or_else(|| {
                json!({
            "mapped":false,"queued":0,"pending":false,"uncertain":false,
            "last_received_at":0,"reply_revision":0})
            });
            value["session_key"] = json!(key);
            value["agent"] = json!(agent);
            value
        })
        .collect();
    Ok(json!({"bridge_account":config.account_id_hex,
        "forwarding_enabled":control.enabled && forwarding, "sessions":sessions}))
}

async fn history(
    config: &crate::BridgeConfig,
    request: &Value,
    reader: crate::reads::Reader,
) -> Result<Value> {
    ensure!(request["version"] == 1, "Unsupported Porter version");
    ensure!(
        request["bridge_account"] == config.account_id_hex,
        "Plugin account changed"
    );
    let socket = Socket {
        path: config.herdr_socket.clone(),
    };
    let agents = herdr::snapshot(&socket).await?;
    let key = request["session_key"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Session required"))?;
    let agent = agents
        .iter()
        .find(|agent| agent.key() == key && request["terminal_id"] == agent.terminal_id)
        .ok_or_else(|| anyhow::anyhow!("Session identity changed"))?;
    let session = agent
        .agent_session
        .as_ref()
        .filter(|session| output::supported(&session.agent))
        .ok_or_else(|| anyhow::anyhow!("Unsupported session reader"))?;
    let history = reader.read(config, session).await?.history;
    // Recheck after slow transcript I/O; never attribute history to a replaced terminal.
    herdr::get(&socket, agent).await?;
    let messages: Vec<_> = history.recent_messages.iter().map(|message| {
        let mut end = message.text.len().min(64000);
        while !message.text.is_char_boundary(end) { end -= 1; }
        json!({"id":message.id,"speaker":message.speaker,"text":&message.text[..end],"truncated":end < message.text.len()})
    }).collect();
    Ok(json!({"session_key":key,"messages":messages}))
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use bech32::ToBase32;

    #[test]
    fn desktop_projection_is_lazy_until_a_subscriber_exists() {
        let root = tempfile::tempdir().unwrap();
        let config: BridgeConfig = serde_json::from_value(json!({
            "phone_npub": bech32::encode("npub", [0x11u8; 32].to_base32(), bech32::Variant::Bech32).unwrap(),
            "account_id_hex": "aa".repeat(32), "herdr_socket":root.path().join("h.sock"),
            "wn_agent_socket":root.path().join("w.sock"), "state_path":root.path().join("state.json")
        })).unwrap();
        let bridge = Bridge::open(config).unwrap();
        publish(&bridge);
        assert!(bridge.porter_updates.borrow().is_null());
        let _subscriber = bridge.porter_updates.subscribe();
        publish(&bridge);
        assert!(bridge.porter_updates.borrow().is_object());
    }
}
