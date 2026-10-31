//! Non-owning local V2 reads. No CLI, health recovery, registration writes, or process signals.
use super::{ExportV2, SessionSnapshot, decision, from_v2, history_from_data, permission, tasks};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Deserialize)]
struct Registration {
    url: String,
    pid: u32,
    password: Option<String>,
}

#[derive(Default)]
struct Cache {
    owner: Option<(String, u32)>,
    cursor: Option<u64>,
    snapshot: Option<SessionSnapshot>,
    messages: Vec<super::MessageV2>,
}

type Sessions = HashMap<(PathBuf, String), Arc<tokio::sync::Mutex<Cache>>>;

const HISTORY_LIMIT: usize = 128 * 1024 * 1024;
const PAGE_SIZE: usize = 100;

#[derive(Deserialize)]
struct Page {
    data: Vec<super::MessageV2>,
    cursor: Cursor,
}

#[derive(Deserialize)]
struct Cursor {
    next: Option<String>,
}

pub struct Reader {
    client: reqwest::Client,
    sessions: Mutex<Sessions>,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(15))
                .build()
                .expect("Local HTTP client configuration"),
            sessions: Mutex::default(),
        }
    }
}

impl Reader {
    pub(crate) fn retain(&self, sessions: &HashSet<String>) {
        if let Ok(mut cache) = self.sessions.lock() {
            cache.retain(|(_, session), _| sessions.contains(session));
        }
    }

    pub async fn read(
        &self,
        config: &crate::BridgeConfig,
        session: &str,
    ) -> Result<SessionSnapshot> {
        ensure!(
            session.starts_with("ses_")
                && session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "Invalid OpenCode session identity"
        );
        let file = config
            .opencode_service_file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OpenCode service registration is not configured"))?;
        // Never include registration contents (which contain authentication) in diagnostics.
        let bytes = tokio::fs::read(file)
            .await
            .map_err(|_| anyhow::anyhow!("OpenCode service registration unavailable"))?;
        ensure!(
            bytes.len() <= 64 * 1024,
            "OpenCode service registration too large"
        );
        let registration: Registration = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid OpenCode service registration"))?;
        let url = reqwest::Url::parse(&registration.url)
            .map_err(|_| anyhow::anyhow!("Invalid OpenCode service address"))?;
        ensure!(
            url.scheme() == "http"
                && url
                    .host_str()
                    .and_then(|host| host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .ok())
                    .is_some_and(|ip| ip.is_loopback())
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/"
                && registration.pid > 0,
            "OpenCode service must use a loopback HTTP address"
        );
        let cached = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("OpenCode reader unavailable"))?;
            sessions
                .entry((file.clone(), session.to_owned()))
                .or_default()
                .clone()
        };
        let mut cache = cached.lock().await;
        let owner = (registration.url.clone(), registration.pid);
        if cache.owner.as_ref() != Some(&owner) {
            *cache = Cache::default();
        }
        // A cursor ahead of the log returns only its current watermark. Event
        // contents are not needed here; never replay an unbounded backlog just
        // to decide whether the projected history needs rebuilding.
        let after = 9_007_199_254_740_991_u64;
        let log = self
            .get(
                &registration,
                &format!("/api/experimental/session/{session}/log?after={after}&follow=false"),
                64 * 1024,
            )
            .await?;
        let cursor = watermark(&log, session)?;
        let mut rebuilt = None;
        let mut snapshot = if cursor.is_some() && cache.snapshot.is_some() && cache.cursor == cursor
        {
            cache.snapshot.clone().unwrap()
        } else {
            let boundary = match (cache.cursor, cursor) {
                (Some(after), Some(through)) if after < through && !cache.messages.is_empty() => {
                    // The public log can omit unsupported event types. Only a
                    // complete, understood delta proves an old prefix is safe.
                    match self
                        .get(
                            &registration,
                            &format!(
                                "/api/experimental/session/{session}/log?after={after}&follow=false"
                            ),
                            1024 * 1024,
                        )
                        .await
                    {
                        Ok(bytes) => {
                            changed_boundary(&bytes, session, after, through, &cache.messages)
                                .ok()
                                .flatten()
                        }
                        Err(_) => None,
                    }
                }
                _ => None,
            };
            let messages = self
                .messages(&registration, session, &cache.messages, boundary)
                .await?;
            if boundary.is_some() {
                let log = self
                    .get(
                        &registration,
                        &format!(
                            "/api/experimental/session/{session}/log?after={after}&follow=false"
                        ),
                        64 * 1024,
                    )
                    .await?;
                ensure!(
                    watermark(&log, session)? == cursor,
                    "OpenCode history changed during incremental read; retry later"
                );
            }
            let export = from_v2(ExportV2 {
                messages: messages.clone(),
            });
            rebuilt = Some(messages);
            SessionSnapshot {
                history: history_from_data(&export),
                tasks: tasks::from_data(&export),
                ..Default::default()
            }
        };
        // Forms and permissions need their own reads: not all changes share the
        // session's durable event log. Failures must not clear pending prompts.
        let forms: Value = serde_json::from_slice(
            &self
                .get(
                    &registration,
                    &format!("/api/session/{session}/form"),
                    1024 * 1024,
                )
                .await?,
        )?;
        snapshot.decision = decision::from_forms(&forms, session)?;
        let permissions: Value = serde_json::from_slice(
            &self
                .get(
                    &registration,
                    &format!("/api/session/{session}/permission"),
                    1024 * 1024,
                )
                .await?,
        )?;
        snapshot.permission = permission::from_response(&permissions, session)?;
        cache.owner = Some(owner);
        cache.cursor = cursor;
        cache.snapshot = Some(snapshot.clone());
        if let Some(messages) = rebuilt {
            cache.messages = messages;
        }
        Ok(snapshot)
    }

    async fn messages(
        &self,
        registration: &Registration,
        session: &str,
        cached: &[super::MessageV2],
        mut boundary: Option<usize>,
    ) -> Result<Vec<super::MessageV2>> {
        let descending = boundary.is_some();
        let mut messages = Vec::new();
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut ids = HashSet::new();
        let mut retained = 0usize;
        loop {
            ensure!(
                cursors.len() < 10_000,
                "OpenCode history has too many pages"
            );
            let mut url = reqwest::Url::parse(&registration.url)?;
            url.set_path(&format!("/api/session/{session}/message"));
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("limit", &PAGE_SIZE.to_string());
                if let Some(cursor) = &cursor {
                    query.append_pair("cursor", cursor);
                } else {
                    query.append_pair("order", if descending { "desc" } else { "asc" });
                }
            }
            let path = format!("{}?{}", url.path(), url.query().unwrap());
            let page: Page =
                serde_json::from_slice(&self.get(registration, &path, HISTORY_LIMIT).await?)?;
            ensure!(
                page.data.len() <= PAGE_SIZE,
                "OpenCode history page exceeds its message limit"
            );
            ensure!(
                !page.data.is_empty() || page.cursor.next.is_none(),
                "Empty OpenCode history page has a continuation"
            );
            for message in page.data {
                if !matches!(message.kind.as_str(), "user" | "assistant") {
                    continue;
                }
                ensure!(
                    !message.id.is_empty() && ids.insert(message.id.clone()),
                    "OpenCode history contains duplicate or missing message identities"
                );
                let message = project(message);
                if let Some(index) = boundary
                    && message.id == cached[index].id
                {
                    if message == cached[index] {
                        // The boundary itself is in ids, so exclude it from
                        // the reused-prefix duplicate check.
                        ensure!(
                            cached[..index].iter().all(|m| !ids.contains(&m.id)),
                            "OpenCode incremental history overlaps its cached prefix"
                        );
                        for old in &cached[..=index] {
                            retained = retained.saturating_add(projected_size(old)?);
                        }
                        ensure!(
                            retained <= HISTORY_LIMIT,
                            "OpenCode projected history exceeds its read limit"
                        );
                        messages.reverse();
                        let mut combined = cached[..=index].to_vec();
                        combined.extend(messages);
                        return Ok(combined);
                    }
                    // The alleged stable boundary changed. Finish reading the
                    // descending timeline rather than trust any cached prefix.
                    boundary = None;
                }
                retained = retained.saturating_add(projected_size(&message)?);
                ensure!(
                    retained <= HISTORY_LIMIT,
                    "OpenCode projected history exceeds its read limit"
                );
                messages.push(message);
            }
            match page.cursor.next {
                None => break,
                Some(next) => {
                    ensure!(
                        next.len() <= 16 * 1024 && !next.is_empty() && cursors.insert(next.clone()),
                        "OpenCode history cursor did not advance"
                    );
                    retained = retained.saturating_add(next.len());
                    ensure!(
                        retained <= HISTORY_LIMIT,
                        "OpenCode projected history exceeds its read limit"
                    );
                    cursor = Some(next);
                }
            }
        }
        if descending {
            messages.reverse();
        }
        Ok(messages)
    }

    async fn get(&self, registration: &Registration, path: &str, max: usize) -> Result<Vec<u8>> {
        let mut request = self
            .client
            .get(format!("{}{path}", registration.url.trim_end_matches('/')));
        if let Some(password) = &registration.password {
            request = request.basic_auth("opencode", Some(password));
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OpenCode read unavailable; retry later"))?;
        ensure!(
            response.status().is_success(),
            "OpenCode read failed (HTTP {})",
            response.status().as_u16()
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= max as u64),
            "OpenCode read exceeded size limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("OpenCode read interrupted; retry later"))?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= max,
                "OpenCode read exceeded size limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

fn project(mut message: super::MessageV2) -> super::MessageV2 {
    // Only error presence affects final-reply selection.
    message.error = message.error.map(|_| Value::Bool(true));
    for part in &mut message.content {
        if part.kind != "text"
            && !(part.kind == "tool" && part.name.as_deref().is_some_and(tasks::tracked))
        {
            // Empty placeholders preserve task revisions based on part position.
            part.kind.clear();
            part.text = None;
            part.id = None;
            part.name = None;
            part.state = Value::Null;
        }
    }
    if message.kind == "user" {
        message.content.clear();
    } else {
        message.text = None;
    }
    message
}

fn projected_size(message: &super::MessageV2) -> Result<usize> {
    let mut size = 256usize
        .saturating_add(message.id.len())
        .saturating_add(message.text.as_ref().map_or(0, String::len));
    for part in &message.content {
        size = size
            .saturating_add(128)
            .saturating_add(part.text.as_ref().map_or(0, String::len))
            .saturating_add(part.id.as_ref().map_or(0, String::len))
            .saturating_add(part.name.as_ref().map_or(0, String::len))
            .saturating_add(serde_json::to_vec(&part.state)?.len());
    }
    Ok(size)
}

/// Return the last provably untouched message before all changed/open messages.
/// Unknown versions, sequence gaps and history rewrites require a full read.
fn changed_boundary(
    bytes: &[u8],
    session: &str,
    after: u64,
    through: u64,
    messages: &[super::MessageV2],
) -> Result<Option<usize>> {
    ensure!(
        watermark(bytes, session)? == Some(through),
        "OpenCode delta watermark changed"
    );
    let mut sequence = after;
    let mut earliest = messages
        .iter()
        .position(|m| m.kind == "assistant" && m.time.completed.is_none())
        .unwrap_or(messages.len());
    for item in log_items(bytes)? {
        if item["type"] == "log.synced" {
            break;
        }
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Invalid OpenCode sequence"))?;
        ensure!(
            item["durable"]["aggregateID"] == session
                && item["data"]["sessionID"] == session
                && item["durable"]["seq"].as_u64() == Some(sequence),
            "Incomplete OpenCode log delta"
        );
        let kind = item["type"].as_str().unwrap_or_default();
        let version = item["durable"]["version"].as_u64();
        let (field, expected_version) = match kind {
            "session.step.started"
            | "session.step.streamed"
            | "session.step.ended"
            | "session.step.failed"
            | "session.text.started"
            | "session.text.ended"
            | "session.reasoning.started"
            | "session.reasoning.ended"
            | "session.tool.input.started"
            | "session.tool.input.ended"
            | "session.tool.called"
            | "session.retry.scheduled" => (Some("assistantMessageID"), 1),
            "session.tool.success" | "session.tool.failed" => (Some("assistantMessageID"), 2),
            "session.message.content.updated" => (Some("messageID"), 1),
            "session.inbox.delivered" => (Some("inboxID"), 1),
            "session.renamed"
            | "session.metadata.updated"
            | "session.permissions"
            | "session.viewed"
            | "session.archived"
            | "session.unarchived"
            | "session.agent.selected"
            | "session.model.selected"
            | "session.inbox.enqueued"
            | "session.inbox.cancelled"
            | "session.inbox.delivery.changed"
            | "session.execution.started"
            | "session.execution.succeeded"
            | "session.execution.failed"
            | "session.execution.interrupted" => (None, 1),
            // Includes reverts, forks, deletion, and future projection contracts.
            _ => return Ok(None),
        };
        if version != Some(expected_version) {
            return Ok(None);
        }
        if let Some(field) = field {
            let id = item["data"][field]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow::anyhow!("OpenCode log message identity missing"))?;
            if let Some(index) = messages.iter().position(|m| m.id == id) {
                earliest = earliest.min(index);
            }
        }
    }
    ensure!(sequence == through, "Incomplete OpenCode log delta");
    Ok(earliest.checked_sub(1))
}

fn log_items(bytes: &[u8]) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let mut data = String::new();
    for line in std::str::from_utf8(bytes)?.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            data.push('\n');
            continue;
        }
        if !line.is_empty() || data.is_empty() {
            continue;
        }
        let item: Value = serde_json::from_str(&data)?;
        data.clear();
        items.push(item);
    }
    ensure!(data.is_empty(), "Incomplete OpenCode log frame");
    Ok(items)
}

fn watermark(bytes: &[u8], session: &str) -> Result<Option<u64>> {
    let mut marker = None;
    for item in log_items(bytes)? {
        ensure!(
            marker.is_none(),
            "Unexpected data after OpenCode log watermark"
        );
        if item["type"] == "log.synced" {
            ensure!(
                item["aggregateID"] == session,
                "OpenCode log belongs to another session"
            );
            let seq = match item.get("seq") {
                None => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("Invalid OpenCode log watermark"))?,
                ),
            };
            marker = Some(seq);
        }
    }
    marker.ok_or_else(|| anyhow::anyhow!("OpenCode log watermark missing"))
}
