use crate::wire::Socket;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
}

impl Workspace {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.workspace_id.is_empty()
                && self.workspace_id.len() <= 128
                && self
                    .workspace_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':'))
                && self.label.len() <= 4096,
            "Invalid workspace information"
        );
        Ok(())
    }

    pub fn display(&self) -> String {
        let label: String = self
            .label
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        format!(
            "{} ({})",
            if label.trim().is_empty() {
                "Unnamed"
            } else {
                label.trim()
            },
            self.workspace_id
        )
    }
}

pub async fn workspaces(socket: &Socket) -> Result<Vec<Workspace>> {
    let response = socket.herdr("workspace.list", json!({})).await?;
    ensure!(
        response["result"]["type"] == "workspace_list",
        "Invalid workspace list response"
    );
    let rows = response["result"]["workspaces"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Workspace list is unavailable"))?;
    ensure!(rows.len() <= 1000, "Workspace list exceeds its limit");
    let workspaces: Vec<Workspace> =
        serde_json::from_value(serde_json::Value::Array(rows.clone()))?;
    let mut seen = HashSet::new();
    for workspace in &workspaces {
        workspace.validate()?;
        ensure!(
            seen.insert(&workspace.workspace_id),
            "Duplicate workspace ID"
        );
    }
    Ok(workspaces)
}

pub async fn workspace(socket: &Socket, id: &str) -> Result<Workspace> {
    let response = socket
        .herdr("workspace.get", json!({"workspace_id":id}))
        .await?;
    ensure!(
        response["result"]["type"] == "workspace_info",
        "Invalid workspace response"
    );
    let workspace: Workspace = serde_json::from_value(response["result"]["workspace"].clone())?;
    workspace.validate()?;
    ensure!(workspace.workspace_id == id, "Workspace identity changed");
    Ok(workspace)
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Session {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Agent {
    pub pane_id: String,
    pub terminal_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub tab_label: Option<String>,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_label: Option<String>,
    #[serde(default)]
    pub agent: String,
    pub agent_status: String,
    pub agent_session: Option<Session>,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    pub terminal_title_stripped: Option<String>,
}

impl Agent {
    pub fn key(&self) -> String {
        match &self.agent_session {
            Some(s) => serde_json::to_string(&(&s.source, &s.agent, &s.kind, &s.value))
                .expect("strings serialize"),
            None => {
                serde_json::to_string(&(&self.terminal_id, &self.agent)).expect("strings serialize")
            }
        }
    }
    pub fn settled(&self) -> bool {
        matches!(self.agent_status.as_str(), "idle" | "done")
    }

    /// Format the introduction label using current bracketed agent codes.
    pub fn title(&self) -> String {
        let raw = self
            .terminal_title_stripped
            .as_deref()
            .unwrap_or(&self.agent);
        let clean: String = raw.chars().filter(|c| !c.is_control()).take(60).collect();
        match clean.split_once(" | ") {
            Some((label, topic))
                if label.len() == 2 && label.bytes().all(|byte| byte.is_ascii_uppercase()) =>
            {
                format!("[{}] {topic}", label.to_ascii_lowercase())
            }
            _ => clean,
        }
    }

    pub fn group_name(&self) -> String {
        self.group_name_with_suffix(None)
    }

    pub fn group_name_with_suffix(&self, suffix: Option<&str>) -> String {
        let fallback = if self.tab_id.is_empty() {
            "session"
        } else {
            self.tab_id.as_str()
        };
        let raw = self
            .tab_label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or(fallback);
        group_name_with_workspace(raw, self.workspace_label.as_deref(), suffix)
    }

    pub fn group_description(&self) -> String {
        let workspace = self
            .workspace_label
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(if self.workspace_id.is_empty() {
                "unknown"
            } else {
                &self.workspace_id
            });
        let agent = self
            .agent_session
            .as_ref()
            .map(|s| s.agent.as_str())
            .unwrap_or(&self.agent);
        let clean = |text: &str| {
            text.chars()
                .filter(|c| !c.is_control())
                .take(500)
                .collect::<String>()
        };
        format!(
            "Hi! This is your chat with {}.\n\nSend /help in any Codr chat to see available commands.\n\nWorkspace: {}\nAgent: {}\nManaged by codr via herdr.",
            clean(&self.group_name()),
            clean(workspace),
            clean(agent)
        )
    }
}

pub fn group_name(label: &str) -> String {
    group_name_with_workspace(label, None, None)
}

pub fn status_label(status: &str) -> String {
    let mut characters = status.chars();
    let punctuation = if matches!(status, "working" | "blocked") {
        "..."
    } else {
        "."
    };
    match characters.next() {
        Some(first) => format!(
            "{}{}{punctuation}",
            first.to_uppercase(),
            characters.as_str()
        ),
        None => "Unknown.".into(),
    }
}

/// `<tab> (<workspace>)`, trimmed to MDK's 256-byte title limit.
fn group_name_with_workspace(label: &str, workspace: Option<&str>, suffix: Option<&str>) -> String {
    const LIMIT: usize = 256;
    let clean = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let mut label = clean(label).trim().to_owned();
    if label.is_empty() {
        label = "session".into();
    }
    let mut workspace = workspace
        .map(|value| clean(value).trim().to_owned())
        .filter(|value| !value.is_empty());
    let suffix = suffix.filter(|s| matches!(*s, "opencode" | "codex" | "claude"));
    let mut name = group_title(&label, workspace.as_deref(), suffix);
    // Keep the tab label primary: shorten the workspace before the tab.
    while name.len() > LIMIT {
        if let Some(value) = workspace.as_mut().filter(|value| !value.is_empty()) {
            value.pop();
            *value = value.trim_end().to_owned();
            name = group_title(&label, Some(value.as_str()), suffix);
        } else if !label.is_empty() {
            label.pop();
            name = group_title(&label, None, suffix);
        } else {
            break;
        }
    }
    name
}

fn group_title(label: &str, workspace: Option<&str>, suffix: Option<&str>) -> String {
    match (workspace.filter(|value| !value.is_empty()), suffix) {
        (Some(workspace), Some(agent)) => format!("{label} ({workspace} - {agent})"),
        (Some(workspace), None) => format!("{label} ({workspace})"),
        (None, Some(agent)) => format!("{} ({agent})", label.trim_end()),
        (None, None) => label.trim_end().to_owned(),
    }
}

pub async fn snapshot(socket: &Socket) -> Result<Vec<Agent>> {
    let response = socket.herdr("session.snapshot", json!({})).await?;
    let agents = &response["result"]["snapshot"]["agents"];
    ensure!(agents.is_array(), "herdr did not return a session snapshot");
    let mut agents: Vec<Agent> = serde_json::from_value(agents.clone())?;
    let labels: HashMap<&str, String> = response["result"]["snapshot"]["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tab| {
            let id = tab["tab_id"].as_str()?;
            let label = tab["label"]
                .as_str()
                .filter(|label| !label.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| tab["number"].as_u64().map(|number| number.to_string()))?;
            Some((id, label))
        })
        .collect();
    for agent in &mut agents {
        if let Some(label) = labels.get(agent.tab_id.as_str()) {
            agent.tab_label = Some(label.clone());
        }
        agent.workspace_label = response["result"]["snapshot"]["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| w["workspace_id"].as_str() == Some(&agent.workspace_id))
            .and_then(|w| w["label"].as_str())
            .map(str::to_owned)
            .or_else(|| agent.workspace_label.clone());
    }
    Ok(agents)
}

pub async fn get(socket: &Socket, agent: &Agent) -> Result<Agent> {
    let response = socket
        .herdr("agent.get", json!({"target":agent.pane_id}))
        .await?;
    let current: Agent = serde_json::from_value(response["result"]["agent"].clone())?;
    ensure!(
        current.key() == agent.key() && current.terminal_id == agent.terminal_id,
        "herdr agent changed"
    );
    Ok(current)
}

pub async fn prompt(socket: &Socket, agent: &Agent, text: &str) -> Result<Value> {
    socket
        .herdr("agent.prompt", json!({"target":agent.pane_id,"text":text}))
        .await
}

pub async fn choose(socket: &Socket, agent: &Agent, choice: usize) -> Result<()> {
    ensure!((1..=9).contains(&choice), "Invalid decision shortcut");
    let response = socket
        .herdr(
            "agent.send_keys",
            json!({"target":agent.pane_id,"keys":[choice.to_string()]}),
        )
        .await?;
    ensure!(
        response["result"]["type"] == "ok",
        "Decision shortcut was not confirmed"
    );
    Ok(())
}
