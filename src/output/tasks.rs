use super::{claude, codex};
use crate::{BridgeConfig, herdr::Session};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Task {
    pub content: String,
    pub status: String,
}

#[derive(Clone, Debug)]
pub struct TaskList {
    pub revision: String,
    pub items: Vec<Task>,
}

impl TaskList {
    pub fn text(&self) -> String {
        let mut text = "> Working...".to_string();
        if !self.items.is_empty() {
            text.push('\n');
        }
        for task in &self.items {
            let mark = match task.status.as_str() {
                "completed" => "✓",
                "in_progress" => "▶",
                "cancelled" => "−",
                _ => "○",
            };
            let line = format!("\n{mark} {}", task.content);
            if text.len() + line.len() > 28000 {
                text.push_str("\n… More tasks omitted.");
                break;
            }
            text.push_str(&line);
        }
        text
    }
}

fn task(content: &Value, status: &Value) -> Option<Task> {
    let content = content.as_str()?.trim();
    let status = status.as_str()?;
    if content.is_empty()
        || !matches!(
            status,
            "pending" | "in_progress" | "completed" | "cancelled"
        )
    {
        return None;
    }
    Some(Task {
        content: content.chars().take(2000).collect(),
        status: status.into(),
    })
}

pub(super) fn tracked(tool: &str) -> bool {
    matches!(
        tool,
        "TodoWrite" | "todowrite" | "TaskCreate" | "TaskUpdate" | "update_plan"
    )
}

#[derive(Default)]
pub(super) struct Tracker {
    pending: HashMap<String, (String, Value)>,
    tasks: Vec<(String, Task)>,
    pub latest: Option<TaskList>,
}

impl Tracker {
    pub fn new_turn(&mut self) {
        self.pending.clear();
        self.latest = None;
    }

    pub fn call(&mut self, id: &str, name: &str, input: &Value) {
        if tracked(name) {
            self.pending.insert(id.into(), (name.into(), input.clone()));
        }
    }

    pub fn complete(&mut self, id: &str, result: &Value, failed: bool, revision: &str) {
        let Some((name, input)) = self.pending.remove(id) else {
            return;
        };
        if failed {
            return;
        }
        match name.as_str() {
            "TodoWrite" | "todowrite" | "update_plan" => {
                let key = if name == "update_plan" {
                    "plan"
                } else {
                    "todos"
                };
                let Some(items) = input[key].as_array().filter(|items| items.len() <= 200) else {
                    return;
                };
                let content = if name == "update_plan" {
                    "step"
                } else {
                    "content"
                };
                let Some(items) = items
                    .iter()
                    .map(|item| task(&item[content], &item["status"]))
                    .collect::<Option<Vec<_>>>()
                else {
                    return;
                };
                self.tasks = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| (index.to_string(), item))
                    .collect();
            }
            "TaskCreate" => {
                let id = result["task"]["id"]
                    .as_str()
                    .or_else(|| result["id"].as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        result
                            .as_str()?
                            .strip_prefix("Task #")?
                            .split_whitespace()
                            .next()
                            .map(str::to_owned)
                    });
                let Some(id) = id.filter(|id| !id.is_empty()) else {
                    return;
                };
                let Some(item) = task(&input["subject"], &Value::String("pending".into())) else {
                    return;
                };
                if self.tasks.len() >= 200 {
                    return;
                }
                self.tasks.push((id, item));
            }
            "TaskUpdate" => {
                let Some(id) = input["taskId"].as_str() else {
                    return;
                };
                if input["status"] == "deleted" {
                    self.tasks.retain(|(key, _)| key != id);
                } else {
                    let Some((_, current)) = self.tasks.iter_mut().find(|(key, _)| key == id)
                    else {
                        return;
                    };
                    let subject = input["subject"].as_str().unwrap_or(&current.content);
                    let status = input["status"].as_str().unwrap_or(&current.status);
                    let Some(updated) = task(
                        &Value::String(subject.into()),
                        &Value::String(status.into()),
                    ) else {
                        return;
                    };
                    *current = updated;
                }
            }
            _ => return,
        }
        self.latest = Some(TaskList {
            revision: revision.into(),
            items: self.tasks.iter().map(|(_, task)| task.clone()).collect(),
        });
    }

    pub fn codex_record(&mut self, row: &Value) {
        let payload = &row["payload"];
        if row["type"] == "event_msg"
            && matches!(
                payload["type"].as_str(),
                Some("task_started" | "turn_started")
            )
        {
            self.new_turn();
        }
        if row["type"] != "response_item" {
            return;
        }
        if payload["type"] == "message" && payload["role"] == "user" {
            self.new_turn();
        }
        if payload["type"] == "function_call" && payload["name"] == "update_plan" {
            if let (Some(id), Some(args)) =
                (payload["call_id"].as_str(), payload["arguments"].as_str())
                && let Ok(args) = serde_json::from_str(args)
            {
                self.call(id, "update_plan", &args);
            }
        } else if payload["type"] == "function_call_output"
            && let Some(id) = payload["call_id"].as_str()
        {
            self.complete(
                id,
                &Value::Null,
                payload["output"] != "Plan updated",
                &format!("codex:{id}"),
            );
        }
    }
}

pub async fn read_session(config: &BridgeConfig, session: &Session) -> Result<Option<TaskList>> {
    ensure!(
        session.kind == "id",
        "Unsupported native session reference kind"
    );
    match session.agent.as_str() {
        "opencode" => {
            let export = super::export_data(&config.opencode_bin, &session.value).await?;
            Ok(from_data(&export))
        }
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
                    codex::snapshot(&home, &id).map(|(_, tasks)| tasks)
                } else {
                    claude::snapshot(&home, &id).map(|(_, tasks)| tasks)
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("Native task reader failed"))?
        }
        _ => Ok(None),
    }
}

pub(super) fn from_data(export: &super::Export) -> Option<TaskList> {
    let mut tracker = Tracker::default();
    for message in &export.messages {
        if message.info.role == "user" {
            tracker.new_turn();
        }
        if message.info.role != "assistant" || message.info.error.is_some() {
            continue;
        }
        for (index, part) in message.parts.iter().enumerate() {
            if part.kind != "tool"
                || part.tool.as_deref() != Some("todowrite")
                || part.state["status"] != "completed"
            {
                continue;
            }
            let revision = format!(
                "opencode:{}:{}",
                message.info.id,
                part.id.clone().unwrap_or_else(|| index.to_string())
            );
            tracker.call(&revision, "todowrite", &part.state["input"]);
            tracker.complete(&revision, &Value::Null, false, &revision);
        }
    }
    tracker.latest
}
