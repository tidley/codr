//! Request-bound OpenCode V2 permissions. Persistent permission rules are never changed.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Permission {
    pub request: Value,
    #[serde(default)]
    pub announced: bool,
    #[serde(default)]
    pub submitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_message_id: Option<String>,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub confirming: bool,
}

#[derive(Clone, Copy)]
pub enum Reply {
    Once,
    Reject,
}

impl Permission {
    pub fn id(&self) -> &str {
        self.request["id"].as_str().unwrap_or("")
    }

    pub fn token(&self) -> String {
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&self.request).expect("JSON serializes"),
        ));
        format!("p{}", &digest[..16])
    }

    pub fn text(&self) -> String {
        let resources = self.request["resources"].as_array().unwrap();
        let action = self.request["action"].as_str().unwrap();
        let mut text = if action == "shell" {
            if resources.len() == 1 {
                "**Run this command?**".to_owned()
            } else {
                "**Run these commands?**".to_owned()
            }
        } else {
            format!("**Allow `{action}` access?**")
        };
        let command = self.request["metadata"]["command"]
            .as_str()
            .filter(|command| !command.is_empty());
        let resource_text: Vec<_> = resources
            .iter()
            .map(|resource| resource.as_str().unwrap())
            .collect();
        let mut body = String::new();
        if let Some(command) = command.filter(|command| *command != resource_text.join("\n")) {
            body.push_str(&format!("Command:\n{command}\n\nRequested access:\n"));
        }
        body.push_str(&resource_text.join("\n"));
        text.push_str(&format!("\n\n```text\n{body}\n```"));
        let mut details = self.request["metadata"].clone();
        if command.is_some()
            && let Some(fields) = details.as_object_mut()
        {
            fields.remove("command");
        }
        if !details.is_null() && details.as_object().is_none_or(|fields| !fields.is_empty()) {
            let details = serde_json::to_string_pretty(&details).expect("JSON serializes");
            text.push_str(&format!("\n\n**Details**\n\n```json\n{details}\n```"));
        }
        text.push_str("\n\nSend a normal message:\n\n1. Allow once\n2. Deny\n\nSaved permissions stay unchanged.");
        text
    }
}

fn identifier(value: &str, prefix: &str) -> Result<()> {
    ensure!(
        value.starts_with(prefix)
            && value.len() <= 128
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
        "Invalid permission identity"
    );
    Ok(())
}

fn safe_display(text: &str) -> bool {
    !text.contains('`') && !text.chars().any(|c| (c.is_control() && !matches!(c, '\n' | '\t')) || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

fn safe_value(value: &Value) -> bool {
    match value {
        Value::String(text) => safe_display(text),
        Value::Array(values) => values.iter().all(safe_value),
        Value::Object(values) => values
            .iter()
            .all(|(key, value)| safe_display(key) && safe_value(value)),
        _ => true,
    }
}

pub fn from_response(value: &Value, session: &str) -> Result<Option<Permission>> {
    identifier(session, "ses_")?;
    let requests = value["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid permissions response"))?;
    ensure!(
        requests.len() <= 1,
        "Multiple permission requests need the terminal"
    );
    let Some(request) = requests.first() else {
        return Ok(None);
    };
    identifier(request["id"].as_str().unwrap_or(""), "per")?;
    ensure!(
        request["sessionID"] == session,
        "Permission belongs to another session"
    );
    ensure!(
        safe_value(request) && serde_json::to_vec(request)?.len() <= 10000,
        "Permission display needs the terminal"
    );
    let action = request["action"].as_str().unwrap_or("");
    ensure!(
        !action.is_empty() && action.len() <= 128 && !action.chars().any(char::is_control),
        "Invalid permission action"
    );
    let resources = request["resources"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid permission resources"))?;
    ensure!(
        !resources.is_empty() && resources.len() <= 8,
        "Permission resources need the terminal"
    );
    let mut bytes = 0;
    for resource in resources {
        let resource = resource
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid permission resource"))?;
        bytes += resource.len();
        ensure!(
            !resource.trim().is_empty()
                && !resource
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t')),
            "Invalid permission resource"
        );
    }
    ensure!(bytes <= 6000, "Large permission requests need the terminal");
    ensure!(
        request["source"]["type"] == "tool"
            && ["messageID", "id"]
                .iter()
                .all(|field| request["source"][field]
                    .as_str()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 256)),
        "Permission has no tool identity"
    );
    Ok(Some(Permission {
        request: request.clone(),
        announced: false,
        submitted: false,
        card_delivery_id: None,
        card_message_id: None,
        changed: false,
        confirming: false,
    }))
}

pub fn is_reply(text: &str, active: bool) -> bool {
    let text = text.trim();
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(first.as_str(), "/allow" | "/reject" | "/allow-always")
        || ["allow", "allow once", "always allow", "deny", "reject"]
            .iter()
            .any(|label| text.eq_ignore_ascii_case(label))
        || (active
            && ((!text.is_empty() && text.bytes().all(|c| c.is_ascii_digit()))
                || text.eq_ignore_ascii_case("confirm")))
}

pub fn parse_reply(text: &str) -> Option<(Reply, &str)> {
    let words: Vec<_> = text.split_whitespace().collect();
    if words.len() != 2 {
        return None;
    }
    let reply = if words[0].eq_ignore_ascii_case("/allow") {
        Reply::Once
    } else if words[0].eq_ignore_ascii_case("/reject") {
        Reply::Reject
    } else {
        return None;
    };
    Some((reply, words[1]))
}

/// Code-free choices apply only to the freshly verified, current permission request.
pub fn parse_card_reply(text: &str) -> Option<Reply> {
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "allow" | "allow once" | "/allow" | "confirm" => Some(Reply::Once),
        "2" | "deny" | "reject" | "/reject" => Some(Reply::Reject),
        _ => None,
    }
}

pub async fn read(bin: &Path, session: &str) -> Result<Option<Permission>> {
    identifier(session, "ses_")?;
    let output = super::run_command(
        bin,
        &["api", "get", &format!("/api/session/{session}/permission")],
        1024 * 1024,
    )
    .await?;
    from_response(
        &serde_json::from_reader(std::io::BufReader::new(output))?,
        session,
    )
}

pub async fn read_request(bin: &Path, session: &str, id: &str) -> Result<Permission> {
    identifier(session, "ses_")?;
    identifier(id, "per")?;
    let output = super::run_command(
        bin,
        &[
            "api",
            "get",
            &format!("/api/session/{session}/permission/{id}"),
        ],
        1024 * 1024,
    )
    .await?;
    let value: Value = serde_json::from_reader(std::io::BufReader::new(output))?;
    from_response(&json!({"data":[value["data"]]}), session)?
        .ok_or_else(|| anyhow::anyhow!("Permission is no longer pending"))
}

pub async fn reply(bin: &Path, session: &str, id: &str, reply: Reply) -> Result<()> {
    identifier(session, "ses_")?;
    identifier(id, "per")?;
    let body = json!({"decision":match reply { Reply::Once => "once", Reply::Reject => "reject" }})
        .to_string();
    let mut output = super::run_command(
        bin,
        &[
            "api",
            "post",
            &format!("/api/session/{session}/permission/{id}/reply"),
            "--data",
            &body,
        ],
        1024 * 1024,
    )
    .await?;
    let mut text = String::new();
    output.read_to_string(&mut text)?;
    ensure!(
        text.trim().is_empty(),
        "Unexpected permission reply response"
    );
    Ok(())
}
