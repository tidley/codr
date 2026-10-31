use super::{FinalMessage, native};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::Instant,
};

struct Row {
    id: String,
    timestamp: Option<i64>,
    parent: Option<String>,
    kind: String,
    same_session: bool,
    private: bool,
    api_error: bool,
    user_prompt: bool,
    input_text: String,
    message: Option<String>,
    stop: Option<String>,
    text: Vec<String>,
    task_calls: Vec<(String, String, Value)>,
    task_results: Vec<(String, Value, bool)>,
}

/// Rows parsed so far from one transcript file. Claude Code only appends, so a read parses the new bytes.
#[derive(Default)]
struct Transcript {
    file: (u64, u64),
    end: u64,
    tail: Vec<u8>,
    rows: Vec<Row>,
    index: HashMap<String, usize>,
    verified: bool,
    task_calls: HashSet<String>,
    used: Option<Instant>,
}

const TAIL_BYTES: u64 = 4096;
const CACHED_TRANSCRIPTS: usize = 32;

static TRANSCRIPTS: LazyLock<Mutex<HashMap<PathBuf, Transcript>>> = LazyLock::new(Default::default);

fn tail(path: &Path, end: u64) -> Result<Vec<u8>> {
    let length = end.min(TAIL_BYTES);
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(end - length))?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn truthy(value: &Value) -> bool {
    !value.is_null() && value != false && value != "" && value != 0
}

#[derive(Default)]
struct Response {
    message: Option<String>,
    timestamp: Option<i64>,
    last_row: String,
    text: Vec<String>,
    complete: bool,
    turn: Option<String>,
}

impl Response {
    fn finish(
        &mut self,
        replies: &mut Vec<FinalMessage>,
        recent: &mut Vec<super::ConversationMessage>,
    ) {
        if self.complete {
            let text = self.text.join("\n");
            if !text.trim().is_empty() {
                super::record_message(
                    recent,
                    "Agent",
                    &format!("claude:{}", self.last_row),
                    &text,
                    self.timestamp,
                );
                replies.push(FinalMessage {
                    id: format!("claude:{}", self.last_row),
                    text,
                    turn: self.turn.clone(),
                });
            }
        }
        *self = Self::default();
    }
}

pub fn read(home: &Path, session: &str) -> Result<Vec<FinalMessage>> {
    snapshot(home, session).map(|(history, _)| history.finals)
}

pub(super) fn snapshot(
    home: &Path,
    session: &str,
) -> Result<(super::SessionHistory, Option<super::tasks::TaskList>)> {
    let session = native::session_id(session)?;
    let name = format!("{session}.jsonl");
    // Only project roots, never nested session/subagent directories. This also handles custom project keys.
    // Claude writes the first transcript only after receiving its first prompt.
    // Adapted from Juraj Bednar's PR #18; scan and ambiguity errors still fail.
    let path = match native::find(&[(home.join("projects"), 1)], |candidate| candidate == name) {
        Err(error) if error.is::<native::NotWritten>() => {
            return Ok((super::SessionHistory::default(), None));
        }
        found => found?,
    };
    let mut transcript = TRANSCRIPTS
        .lock()
        .unwrap()
        .remove(&path)
        .unwrap_or_default();
    transcript.update(&path, &session)?;
    let snapshot = transcript.snapshot().map(|(mut history, tasks)| {
        history.activity_at = native::modified_at(&path);
        (history, tasks)
    });
    transcript.used = Some(Instant::now());
    let mut cached = TRANSCRIPTS.lock().unwrap();
    if cached.len() >= CACHED_TRANSCRIPTS
        && let Some(oldest) = cached
            .iter()
            .min_by_key(|(_, transcript)| transcript.used)
            .map(|(path, _)| path.clone())
    {
        cached.remove(&oldest);
    }
    cached.insert(path, transcript);
    snapshot
}

impl Transcript {
    fn update(&mut self, path: &Path, session: &str) -> Result<()> {
        let metadata = std::fs::metadata(path)?;
        let file = (metadata.dev(), metadata.ino());
        if file != self.file || tail(path, self.end)? != self.tail {
            *self = Self {
                file,
                ..Self::default()
            };
        }
        let Self {
            rows,
            index,
            verified,
            task_calls,
            ..
        } = self;
        let end = native::records_from(path, self.end, |value| {
            let same_session = value["sessionId"]
                .as_str()
                .and_then(|id| native::session_id(id).ok())
                .as_deref()
                == Some(session);
            *verified |= same_session;
            let Some(id) = value["uuid"].as_str().filter(|id| !id.is_empty()) else {
                return Ok(());
            };
            let Some(kind @ ("user" | "assistant" | "progress" | "system" | "attachment")) =
                value["type"].as_str()
            else {
                return Ok(());
            };
            let private = truthy(&value["isSidechain"])
                || truthy(&value["isMeta"])
                || truthy(&value["teamName"]);
            let content = &value["message"]["content"];
            let blocks = content.as_array();
            // Claude Code records background-task notifications as system-sourced user rows.
            let system_prompt = value["promptSource"] == "system"
                || value["origin"]["kind"]
                    .as_str()
                    .is_some_and(|origin| origin != "human");
            let user_prompt = kind == "user"
                && !private
                && !system_prompt
                && !truthy(&value["isCompactSummary"])
                && (content.is_string()
                    || blocks.is_some_and(|parts| {
                        parts.iter().any(|part| {
                            matches!(part["type"].as_str(), Some("text" | "image" | "document"))
                        })
                    }));
            let text = if kind == "assistant" && !private && value["message"]["role"] == "assistant"
            {
                blocks
                    .map(|parts| {
                        parts
                            .iter()
                            .filter(|part| part["type"] == "text")
                            .filter_map(|part| part["text"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut row = Row {
                id: id.into(),
                timestamp: super::source_timestamp(&value["timestamp"]),
                parent: value["parentUuid"].as_str().map(str::to_owned),
                kind: kind.into(),
                same_session,
                private,
                api_error: truthy(&value["isApiErrorMessage"])
                    || !value["message"]["error"].is_null(),
                user_prompt,
                input_text: if user_prompt {
                    content.as_str().map(str::to_owned).unwrap_or_else(|| {
                        blocks
                            .into_iter()
                            .flatten()
                            .filter(|part| part["type"] == "text")
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                } else {
                    String::new()
                },
                message: value["message"]["id"].as_str().map(str::to_owned),
                stop: value["message"]["stop_reason"].as_str().map(str::to_owned),
                text,
                task_calls: Vec::new(),
                task_results: Vec::new(),
            };
            for part in blocks.into_iter().flatten() {
                if part["type"] == "tool_use" && kind == "assistant" {
                    if let (Some(id), Some(name)) = (part["id"].as_str(), part["name"].as_str())
                        && super::tasks::tracked(name)
                    {
                        task_calls.insert(id.into());
                        row.task_calls
                            .push((id.into(), name.into(), part["input"].clone()));
                    }
                } else if part["type"] == "tool_result"
                    && kind == "user"
                    && let Some(id) = part["tool_use_id"].as_str()
                    && task_calls.contains(id)
                {
                    let result = if blocks.is_some_and(|parts| parts.len() == 1)
                        && !value["toolUseResult"].is_null()
                    {
                        &value["toolUseResult"]
                    } else {
                        &part["content"]
                    };
                    row.task_results
                        .push((id.into(), result.clone(), part["is_error"] == true));
                }
            }
            index.insert(row.id.clone(), rows.len());
            rows.push(row);
            Ok(())
        })?;
        self.end = end;
        self.tail = tail(path, end)?;
        Ok(())
    }

    fn snapshot(&self) -> Result<(super::SessionHistory, Option<super::tasks::TaskList>)> {
        let (rows, index) = (&self.rows, &self.index);
        ensure!(
            self.verified,
            "Claude transcript session identity mismatch or missing metadata"
        );
        let parents: HashSet<_> = index
            .values()
            .filter(|position| !rows[**position].private && rows[**position].same_session)
            .filter_map(|position| rows[*position].parent.as_deref())
            .collect();
        let mut leaves = Vec::new();
        for position in index
            .values()
            .copied()
            .filter(|position| !parents.contains(rows[*position].id.as_str()))
        {
            let mut current = Some(position);
            let mut seen = HashSet::new();
            while let Some(position) = current {
                ensure!(seen.insert(position), "Claude transcript parent cycle");
                let row = &rows[position];
                if matches!(row.kind.as_str(), "user" | "assistant") {
                    if !row.private && row.same_session {
                        leaves.push(position);
                    }
                    break;
                }
                current = row
                    .parent
                    .as_ref()
                    .and_then(|parent| index.get(parent))
                    .copied();
            }
        }
        // Prefer the latest main-conversation message by file position, not a sidechain or a timestamp.
        let Some(mut current) = leaves.into_iter().max() else {
            return Ok((super::SessionHistory::default(), None));
        };
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        loop {
            ensure!(seen.insert(current), "Claude transcript parent cycle");
            chain.push(current);
            let Some(parent) = rows[current]
                .parent
                .as_ref()
                .and_then(|parent| index.get(parent))
            else {
                break;
            };
            current = *parent;
        }
        let mut response = Response::default();
        let mut replies = Vec::new();
        let mut tasks = super::tasks::Tracker::default();
        let mut latest_turn = None;
        let mut turns = std::collections::BTreeSet::new();
        let mut user_inputs = Vec::new();
        let mut recent_messages = Vec::new();
        for position in chain.into_iter().rev() {
            let row = &rows[position];
            if row.private || !row.same_session {
                continue;
            }
            if row.user_prompt {
                user_inputs.push(crate::activity::UserInput::new(
                    format!("claude:{}", row.id),
                    &row.input_text,
                ));
                response.finish(&mut replies, &mut recent_messages);
                super::record_message(
                    &mut recent_messages,
                    "You",
                    &format!("claude:{}", row.id),
                    &row.input_text,
                    row.timestamp,
                );
                latest_turn = Some(format!("claude:{}", row.id));
                turns.insert(latest_turn.clone().unwrap());
                tasks.new_turn();
            }
            if !row.api_error {
                for (id, name, input) in &row.task_calls {
                    tasks.call(id, name, input);
                }
                for (id, result, failed) in &row.task_results {
                    tasks.complete(id, result, *failed, &format!("claude:{}:{id}", row.id));
                }
            }
            if row.kind != "assistant" {
                continue;
            }
            if row.api_error {
                response = Response::default();
                continue;
            }
            let message = row.message.as_deref().unwrap_or(&row.id);
            if response.message.as_deref() != Some(message) {
                // A continuation for the same user turn supersedes an incomplete model response.
                // A completed one stays final, e.g. a reply before a background task reports back.
                response.finish(&mut replies, &mut recent_messages);
                response = Response {
                    message: Some(message.into()),
                    turn: latest_turn.clone(),
                    ..Response::default()
                };
            }
            response.last_row = row.id.clone();
            response.timestamp = response.timestamp.or(row.timestamp);
            response.text.extend(row.text.iter().cloned());
            // Duration/system markers and null stop reasons do not prove a successful final answer.
            response.complete = row.stop.as_deref() == Some("end_turn");
        }
        response.finish(&mut replies, &mut recent_messages);
        Ok((
            super::SessionHistory {
                activity_at: None,
                finals: replies,
                latest_turn,
                turns: turns.into_iter().collect(),
                user_inputs,
                recent_messages: recent_messages[recent_messages.len().saturating_sub(3)..]
                    .to_vec(),
                last_messages: recent_messages,
            },
            tasks.latest,
        ))
    }
}
