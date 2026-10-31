use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use tokio::process::Command;

pub mod claude;
pub mod codex;
pub mod decision;
mod native;
pub mod opencode;
pub mod permission;
pub mod service;
pub mod tasks;

pub fn supported(agent: &str) -> bool {
    matches!(agent, "opencode" | "codex" | "claude")
}

pub async fn read_session(
    config: &crate::config::BridgeConfig,
    session: &crate::herdr::Session,
) -> Result<Vec<FinalMessage>> {
    Ok(read_history(config, session).await?.finals)
}

#[derive(Clone, Debug, Default)]
pub struct SessionHistory {
    /// Unix seconds of source activity; never the time this bridge polled it.
    pub activity_at: Option<u64>,
    pub finals: Vec<FinalMessage>,
    pub latest_turn: Option<String>,
    pub turns: Vec<String>,
    pub user_inputs: Vec<crate::activity::UserInput>,
    pub recent_messages: Vec<ConversationMessage>,
    /// Bounded replay window for explicit /last requests; not a delivery cursor.
    pub last_messages: Vec<ConversationMessage>,
}

pub(crate) const MAX_LAST_MESSAGES: usize = 100;

#[derive(Clone, Debug)]
pub struct ConversationMessage {
    pub id: String,
    pub speaker: &'static str,
    pub text: String,
    /// Unix seconds from the source message, not the catch-up request or file mtime.
    pub timestamp: Option<i64>,
}

impl ConversationMessage {
    pub fn key(&self) -> String {
        message_key(self.speaker, &self.id, &self.text)
    }
}

pub(crate) fn message_key(speaker: &str, id: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(speaker, id, text)).expect("strings serialize"),
    ))
}

impl SessionHistory {
    /// A bounded conversation excerpt; never prompts an agent or includes tool output.
    pub fn catch_up(&self) -> Option<String> {
        Self::excerpt(&self.recent_messages)
    }

    pub fn last(&self, count: usize) -> Option<String> {
        Self::excerpt(&self.last_messages[self.last_messages.len().saturating_sub(count)..])
    }

    fn excerpt(messages: &[ConversationMessage]) -> Option<String> {
        (!messages.is_empty()).then(|| {
            messages
                .iter()
                .map(|message| {
                    let date = message
                        .timestamp
                        .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                        .map(|at| at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_else(|| "Date unknown".into());
                    format!("**{}:**\n_{date}_\n{}", message.speaker, message.text)
                })
                .collect::<Vec<_>>()
                .join("\n\n---\n\n")
        })
    }
}

fn record_message(
    recent: &mut Vec<ConversationMessage>,
    speaker: &'static str,
    id: &str,
    text: &str,
    timestamp: Option<i64>,
) {
    if !id.is_empty() && !text.trim().is_empty() {
        recent.push(ConversationMessage {
            id: id.into(),
            speaker,
            text: text.into(),
            timestamp,
        });
        if recent.len() > MAX_LAST_MESSAGES {
            recent.remove(0);
        }
    }
}

fn source_timestamp(value: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|at| at.timestamp())
}

pub async fn read_history(
    config: &crate::config::BridgeConfig,
    session: &crate::herdr::Session,
) -> Result<SessionHistory> {
    ensure!(
        session.kind == "id",
        "Unsupported native session reference kind"
    );
    match session.agent.as_str() {
        "opencode" if config.opencode_db.is_some() || config.opencode_service_file.is_some() => {
            Ok(read_snapshot(config, session).await?.history)
        }
        "opencode" => Ok(select_history(
            export_data(&config.opencode_bin, &session.value).await?,
        )),
        "codex" | "claude" => {
            let provider = session.agent.clone();
            let id = session.value.clone();
            let home = if provider == "codex" {
                config.codex_home.clone()
            } else {
                config.claude_home.clone()
            };
            tokio::task::spawn_blocking(move || {
                if provider == "codex" {
                    codex::snapshot(&home, &id).map(|(history, _)| history)
                } else {
                    claude::snapshot(&home, &id).map(|(history, _)| history)
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("Native transcript reader failed"))?
        }
        _ => Ok(SessionHistory::default()),
    }
}

#[derive(Clone, Debug)]
pub struct FinalMessage {
    pub id: String,
    pub text: String,
    pub turn: Option<String>,
}

// Unknown fields include tool outputs, attachments and token accounting. Serde skips
// them directly from the reader instead of retaining the complete export in memory.
#[derive(Deserialize)]
struct Export {
    messages: Vec<Message>,
}
#[derive(Clone, Deserialize)]
struct Message {
    info: Info,
    parts: Vec<Part>,
}
#[derive(Clone, Deserialize)]
struct Info {
    #[serde(default)]
    id: String,
    role: String,
    #[serde(default)]
    time: MessageTime,
    finish: Option<String>,
    error: Option<Value>,
    #[serde(default)]
    summary: Value,
    #[serde(default, rename = "parentID")]
    parent_id: Option<String>,
}
#[derive(Clone, Default, Deserialize)]
struct MessageTime {
    created: Option<u64>,
    updated: Option<u64>,
    completed: Option<u64>,
}
#[derive(Clone, Deserialize)]
struct Part {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    #[serde(default)]
    synthetic: bool,
    #[serde(default)]
    ignored: bool,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    state: Value,
}

pub fn final_messages_from_reader(reader: impl Read) -> Result<Vec<FinalMessage>> {
    select_finals(serde_json::from_reader(reader)?)
}

pub fn final_messages(export: &Value) -> Result<Vec<FinalMessage>> {
    select_finals(serde_json::from_value(export.clone())?)
}

pub fn final_messages_v2(export: &Value) -> Result<Vec<FinalMessage>> {
    select_finals(from_v2(serde_json::from_value(export.clone())?))
}

fn select_finals(export: Export) -> Result<Vec<FinalMessage>> {
    Ok(select_history(export).finals)
}

fn select_history(export: Export) -> SessionHistory {
    history_from_data(&export)
}

fn history_from_data(export: &Export) -> SessionHistory {
    let mut out = vec![];
    let mut latest_turn = None;
    let mut turns = std::collections::BTreeSet::new();
    let mut has_user = false;
    let mut user_inputs = Vec::new();
    let mut recent_messages = Vec::new();
    for message in &export.messages {
        let info = &message.info;
        let timestamp = info
            .time
            .created
            .or(info.time.completed)
            .and_then(|milliseconds| i64::try_from(milliseconds / 1000).ok());
        if info.role == "user" {
            let parts: Vec<_> = message
                .parts
                .iter()
                .filter(|part| part.kind == "text" && !part.synthetic && !part.ignored)
                .collect();
            if !info.id.is_empty()
                && info.summary != true
                && (message.parts.is_empty()
                    || !parts.is_empty()
                    || message
                        .parts
                        .iter()
                        .any(|part| matches!(part.kind.as_str(), "file" | "image")))
            {
                let text = parts
                    .iter()
                    .filter_map(|part| part.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                let id = format!("opencode:{}", info.id);
                record_message(&mut recent_messages, "You", &info.id, &text, timestamp);
                user_inputs.push(if message.parts.is_empty() {
                    crate::activity::UserInput::pending(id)
                } else {
                    crate::activity::UserInput::new(id, &text)
                });
            }
            latest_turn = Some(format!("opencode:{}", info.id));
            turns.insert(latest_turn.clone().unwrap());
            has_user = true;
            continue;
        }
        let turn = info
            .parent_id
            .as_ref()
            .map(|id| format!("opencode:{id}"))
            .or_else(|| latest_turn.clone());
        if let Some(turn) = &turn {
            turns.insert(turn.clone());
        }
        if !has_user && turn.is_some() {
            latest_turn = turn.clone();
        }
        if info.role != "assistant"
            || info.time.completed.is_none()
            || info.finish.as_deref() != Some("stop")
            || info.error.is_some()
            || info.summary == true
        {
            continue;
        }
        let text = message
            .parts
            .iter()
            .filter(|p| p.kind == "text" && !p.synthetic && !p.ignored)
            .filter_map(|p| p.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            record_message(&mut recent_messages, "Agent", &info.id, &text, timestamp);
            out.push(FinalMessage {
                id: info.id.clone(),
                text,
                turn,
            });
        }
    }
    SessionHistory {
        activity_at: export
            .messages
            .iter()
            .filter(|message| {
                matches!(message.info.role.as_str(), "user" | "assistant")
                    && message.info.summary != true
            })
            .flat_map(|message| {
                [
                    message.info.time.created,
                    message.info.time.updated,
                    message.info.time.completed,
                ]
            })
            .flatten()
            .max()
            .map(|milliseconds| milliseconds / 1000),
        finals: out,
        latest_turn,
        turns: turns.into_iter().collect(),
        user_inputs,
        recent_messages: recent_messages[recent_messages.len().saturating_sub(3)..].to_vec(),
        last_messages: recent_messages,
    }
}

pub async fn export(bin: &Path, session: &str) -> Result<Vec<FinalMessage>> {
    select_finals(export_data(bin, session).await?)
}

const EXPORT_UNKNOWN: u8 = 0;
const EXPORT_V1: u8 = 1;
const EXPORT_V2: u8 = 2;
// Bounded CLI fallback retries adapted from Juraj Bednar's afe4f38 contribution.
const EXPORT_ATTEMPTS: usize = 3;
const EXPORT_RETRY_DELAY: Duration = Duration::from_millis(200);

/// The first successful command selects the API for later reads. OpenCode 2.x removed
/// the v1 `--pure export` command, so one failed attempt is expected after an upgrade.
static EXPORT_API: AtomicU8 = AtomicU8::new(EXPORT_UNKNOWN);

#[derive(Debug, thiserror::Error)]
#[error("Configured OpenCode executable is missing; rerun setup")]
struct MissingExecutable;

#[derive(Debug, thiserror::Error)]
#[error("OpenCode export ended before its document was complete: {0}")]
struct IncompleteExport(serde_json::Error);

#[derive(Debug, thiserror::Error)]
#[error("OpenCode export failed (v1: {v1}; v2: {v2})")]
struct ExportFailed {
    v1: String,
    v2: String,
    incomplete: bool,
}

fn export_parse_error(error: serde_json::Error) -> anyhow::Error {
    if error.is_eof() {
        IncompleteExport(error).into()
    } else {
        error.into()
    }
}

async fn export_data(bin: &Path, session: &str) -> Result<Export> {
    Ok(export_with_api(bin, session).await?.0)
}

async fn export_with_api(bin: &Path, session: &str) -> Result<(Export, u8)> {
    if bin.is_absolute() && !bin.is_file() {
        return Err(MissingExecutable.into());
    }
    let attempts = if EXPORT_API.load(Ordering::Relaxed) == EXPORT_V2 {
        [EXPORT_V2, EXPORT_V1]
    } else {
        [EXPORT_V1, EXPORT_V2]
    };
    let mut failures = [String::new(), String::new()];
    let mut incomplete = false;
    for api in attempts {
        for attempt in 1..=EXPORT_ATTEMPTS {
            match run_export(bin, session, api).await {
                Ok(export) => {
                    EXPORT_API.store(api, Ordering::Relaxed);
                    return Ok((export, api));
                }
                Err(error) => {
                    let retryable = error.is::<IncompleteExport>();
                    incomplete |= retryable;
                    failures[usize::from(api - EXPORT_V1)] = format!("{error:#}");
                    if !retryable || attempt == EXPORT_ATTEMPTS {
                        break;
                    }
                }
            }
            tokio::time::sleep(EXPORT_RETRY_DELAY).await;
        }
    }
    let [v1, v2] = failures;
    Err(ExportFailed { v1, v2, incomplete }.into())
}

async fn run_export(bin: &Path, session: &str, api: u8) -> Result<Export> {
    let args: &[&str] = if api == EXPORT_V2 {
        &["session", "export", session]
    } else {
        &["--pure", "export", session]
    };
    let output = run_command(bin, args, 128 * 1024 * 1024).await?;
    match api {
        EXPORT_V2 => tokio::task::spawn_blocking(move || {
            Ok(from_v2(
                serde_json::from_reader(BufReader::new(output)).map_err(export_parse_error)?,
            ))
        })
        .await
        .map_err(|_| anyhow::anyhow!("OpenCode export reader failed"))?,
        _ => tokio::task::spawn_blocking(move || {
            serde_json::from_reader(BufReader::new(output)).map_err(export_parse_error)
        })
        .await
        .map_err(|_| anyhow::anyhow!("OpenCode export reader failed"))?,
    }
}

pub(crate) async fn run_command(
    bin: &Path,
    args: &[&str],
    max_bytes: u64,
) -> Result<std::fs::File> {
    // Regular files avoid OpenCode's observed stdout-pipe truncation. They are anonymous and owner-only.
    let mut output = tempfile::tempfile()?;
    let mut errors = tempfile::tempfile()?;
    let mut command = Command::new(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(errors.try_clone()?)
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(30), command.status()).await??;
    if !status.success() {
        errors.seek(SeekFrom::Start(0))?;
        let mut message = String::new();
        errors.take(4096).read_to_string(&mut message)?;
        let message = message.trim();
        anyhow::bail!(
            "exited with {}: {}",
            status,
            if message.is_empty() {
                "no error output"
            } else {
                message
            }
        );
    }
    ensure!(
        output.metadata()?.len() <= max_bytes,
        "OpenCode output exceeds its read limit"
    );
    output.seek(SeekFrom::Start(0))?;
    Ok(output)
}

#[derive(Deserialize)]
struct ExportV2 {
    messages: Vec<MessageV2>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct MessageV2 {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    content: Vec<PartV2>,
    #[serde(default)]
    time: MessageTimeV2,
    #[serde(default)]
    finish: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Clone, Default, Deserialize, PartialEq)]
struct MessageTimeV2 {
    created: Option<u64>,
    updated: Option<u64>,
    streamed: Option<u64>,
    #[serde(default)]
    completed: Option<u64>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct PartV2 {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    state: Value,
}

/// OpenCode 2.x exports flattened session events instead of v1 messages with parts.
fn from_v2(export: ExportV2) -> Export {
    let messages = export
        .messages
        .into_iter()
        .filter_map(|message| {
            let (role, parts, finish, completed) = match message.kind.as_str() {
                "user" => (
                    "user",
                    vec![Part {
                        kind: "text".into(),
                        text: message.text,
                        synthetic: false,
                        ignored: false,
                        id: None,
                        tool: None,
                        state: Value::Null,
                    }],
                    None,
                    None,
                ),
                "assistant" => (
                    "assistant",
                    message.content.into_iter().map(part_from_v2).collect(),
                    message.finish,
                    message.time.completed,
                ),
                // Subagent markers and idle events are not conversation messages.
                _ => return None,
            };
            Some(Message {
                info: Info {
                    id: message.id,
                    role: role.into(),
                    time: MessageTime {
                        completed,
                        created: message.time.created,
                        updated: message
                            .time
                            .updated
                            .into_iter()
                            .chain(message.time.streamed)
                            .max(),
                    },
                    finish,
                    error: message.error,
                    summary: Value::Bool(false),
                    parent_id: None,
                },
                parts,
            })
        })
        .collect();
    Export { messages }
}

fn part_from_v2(part: PartV2) -> Part {
    Part {
        kind: part.kind,
        text: part.text,
        synthetic: false,
        ignored: false,
        id: part.id,
        tool: part.name,
        state: part.state,
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionSnapshot {
    pub history: SessionHistory,
    pub tasks: Option<tasks::TaskList>,
    pub decision: Option<decision::Decision>,
    pub permission: Option<permission::Permission>,
}

/// Fixed diagnostics never include transcript content, IDs, paths, or CLI stderr.
pub fn read_error_message(error: &anyhow::Error) -> &'static str {
    if error.is::<IncompleteExport>()
        || error
            .downcast_ref::<ExportFailed>()
            .is_some_and(|failure| failure.incomplete)
    {
        return "OpenCode export stayed incomplete after retries";
    }
    if error.downcast_ref::<MissingExecutable>().is_some() {
        return "configured OpenCode executable is missing; rerun setup";
    }
    if let Some(rusqlite::Error::SqliteFailure(code, _)) = error.downcast_ref::<rusqlite::Error>() {
        return match code.code {
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                "database is busy or locked"
            }
            rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::PermissionDenied => {
                "database access failed"
            }
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                "database is corrupt or invalid"
            }
            _ => "database read failed",
        };
    }
    "transcript read failed; verify CLI export support and transcript contents"
}

pub(crate) async fn read_opencode_snapshot(
    config: &crate::BridgeConfig,
    session: &crate::herdr::Session,
    database: std::sync::Arc<std::sync::Mutex<opencode::Reader>>,
) -> Result<SessionSnapshot> {
    if config.opencode_service_file.is_some() {
        return service::Reader::default()
            .read(config, &session.value)
            .await;
    }
    if let Some(path) = &config.opencode_db {
        let path = path.clone();
        let id = session.value.clone();
        let read = tokio::task::spawn_blocking(move || {
            database
                .lock()
                .map_err(|_| anyhow::anyhow!("OpenCode database reader failed"))?
                .read_compatible(&path, &id)
        })
        .await
        .map_err(|_| anyhow::anyhow!("OpenCode database reader failed"))??;
        if let Some(read) = read {
            return Ok(read.snapshot);
        }
    }
    let (export, api) = export_with_api(&config.opencode_bin, &session.value).await?;
    Ok(SessionSnapshot {
        history: history_from_data(&export),
        tasks: tasks::from_data(&export),
        decision: if api == EXPORT_V2 {
            decision::read_v2(&config.opencode_bin, &session.value)
                .await
                .ok()
                .flatten()
        } else {
            decision::from_data(&export).ok().flatten()
        },
        permission: if api == EXPORT_V2 {
            permission::read(&config.opencode_bin, &session.value)
                .await
                .ok()
                .flatten()
        } else {
            None
        },
    })
}

/// Read reply, task and decision projections from one consistent transcript.
pub async fn read_snapshot(
    config: &crate::BridgeConfig,
    session: &crate::herdr::Session,
) -> Result<SessionSnapshot> {
    ensure!(
        session.kind == "id",
        "Unsupported native session reference kind"
    );
    match session.agent.as_str() {
        "opencode" => read_opencode_snapshot(config, session, Default::default()).await,
        "codex" | "claude" => {
            let provider = session.agent.clone();
            let id = session.value.clone();
            let home = if provider == "codex" {
                config.codex_home.clone()
            } else {
                config.claude_home.clone()
            };
            tokio::task::spawn_blocking(move || {
                let (history, tasks) = if provider == "codex" {
                    codex::snapshot(&home, &id)?
                } else {
                    claude::snapshot(&home, &id)?
                };
                Ok(SessionSnapshot {
                    history,
                    tasks,
                    decision: None,
                    permission: None,
                })
            })
            .await
            .map_err(|_| anyhow::anyhow!("Native transcript reader failed"))?
        }
        _ => Ok(SessionSnapshot::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_absolute_executable_is_reported_clearly() {
        let error = match export_data(Path::new("/nonexistent/opencode"), "ses_test").await {
            Ok(_) => panic!("missing executable must fail"),
            Err(error) => error,
        };
        assert_eq!(
            read_error_message(&error),
            "configured OpenCode executable is missing; rerun setup"
        );
    }
}
