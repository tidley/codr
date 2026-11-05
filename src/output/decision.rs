use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_width::UnicodeWidthStr;

fn custom_default() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct OptionItem {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Question {
    pub question: String,
    #[serde(default)]
    pub header: String,
    pub options: Vec<OptionItem>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default = "custom_default")]
    pub custom: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Decision {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_id: Option<String>,
    pub questions: Vec<Question>,
    #[serde(default)]
    pub announced: bool,
    #[serde(default)]
    pub submitted: bool,
    #[serde(default)]
    pub requires_token: bool,
}

#[derive(Debug, PartialEq)]
pub enum Answer {
    Choice(usize),
    Custom(String),
}

fn question_code(word: &str) -> bool {
    matches!(word.len(), 7 | 17)
        && word.as_bytes()[0].eq_ignore_ascii_case(&b'q')
        && word.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl Decision {
    pub fn same_request(&self, other: &Self) -> bool {
        self.id == other.id && self.form_id == other.form_id && self.questions == other.questions
    }

    pub fn token(&self) -> String {
        if let Some(form) = &self.form_id {
            let identity = serde_json::to_vec(&(&self.id, form, &self.questions))
                .expect("Question identity serializes");
            let digest = hex::encode(Sha256::digest(identity));
            return format!("q{}", &digest[..16]);
        }
        let tail: String = self.id.chars().rev().take(6).collect();
        format!("q{}", tail.chars().rev().collect::<String>().to_lowercase())
    }

    pub fn supported(&self) -> bool {
        self.questions.len() == 1
            && !self.questions[0].multiple
            && (1..=9).contains(&self.questions[0].options.len())
    }

    pub fn supports_custom(&self) -> bool {
        self.supported() && self.questions[0].custom && self.form_id.is_some()
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        for (index, question) in self.questions.iter().enumerate() {
            if index > 0 {
                text.push_str("\n\n");
            }
            text.push_str(&question.question);
            for (index, option) in question.options.iter().enumerate() {
                text.push_str(&format!("\n\n{}. {}", index + 1, option.label));
                if !option.description.is_empty() {
                    text.push_str(&format!("\n{}", option.description));
                }
            }
        }
        if self.submitted {
            text.push_str("\n\nAn answer was already submitted. Check the terminal if this question is still waiting.");
        } else if !self.supported() {
            text.push_str("\n\nAnswer this multi-question or multi-select dialog in the terminal.");
        } else if self.requires_token {
            text.push_str(&format!(
                "\n\nThe question changed. Reply with {} and your choice, for example: {} 1",
                self.token(),
                self.token()
            ));
            if self.supports_custom() {
                text.push_str(" A custom text answer can follow the same question code.");
            }
        } else if self.questions[0].custom && !self.supports_custom() {
            text.push_str("\n\nUse the terminal for a custom answer.");
        }
        text
    }

    fn answer_text<'a>(&self, text: &'a str) -> Result<&'a str> {
        let text = text.trim();
        let text = text.strip_prefix("/answer ").unwrap_or(text).trim();
        let mut words = text.splitn(2, char::is_whitespace);
        let first = words.next().unwrap_or("").trim_end_matches(':');
        let token_present = first.eq_ignore_ascii_case(&self.token());
        ensure!(
            !question_code(first) || token_present,
            "That question code is stale. Use the code on the current question card."
        );
        let answer = if token_present {
            words.next().unwrap_or("").trim()
        } else {
            text
        };
        ensure!(
            !self.requires_token || token_present,
            "The question changed. Include its question code with your answer."
        );
        Ok(answer)
    }

    pub fn choice(&self, text: &str) -> Result<usize> {
        ensure!(
            self.supported(),
            "This dialog needs an answer in the terminal."
        );
        let answer = self.answer_text(text)?;
        if !answer.is_empty()
            && answer.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(index) = answer.parse::<usize>()
            && (1..=self.questions[0].options.len()).contains(&index)
        {
            return Ok(index);
        }
        let matches: Vec<_> = self.questions[0]
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| option.label.to_lowercase() == answer.to_lowercase())
            .collect();
        if matches.len() == 1 {
            return Ok(matches[0].0 + 1);
        }
        bail!(
            "Reply with a listed option number or its label. Use the terminal for a custom answer."
        )
    }

    pub fn answer(&self, text: &str) -> Result<Answer> {
        if let Ok(choice) = self.choice(text) {
            return Ok(Answer::Choice(choice));
        }
        ensure!(
            self.supports_custom(),
            "Reply with a listed option number or its label. Custom answers for this dialog need the terminal."
        );
        let answer = self.answer_text(text)?;
        ensure!(
            !answer.is_empty() && answer.len() <= 4000,
            "Custom answers must contain 1–4000 UTF-8 bytes."
        );
        ensure!(
            !text.trim().starts_with('/') || text.trim().starts_with("/answer "),
            "Use /answer TEXT to send a custom answer beginning with '/'."
        );
        Ok(Answer::Custom(answer.to_owned()))
    }

    pub fn looks_like_answer(&self, text: &str) -> bool {
        self.looks_like_explicit_answer(text)
            || (self.supports_custom() && !text.trim().is_empty() && !text.trim().starts_with('/'))
    }

    pub fn looks_like_explicit_answer(&self, text: &str) -> bool {
        let text = text.trim();
        let first = text
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(':');
        let coded = question_code(first);
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            || text.starts_with("/answer ")
            || first.eq_ignore_ascii_case(&self.token())
            || coded
            || self
                .questions
                .iter()
                .flat_map(|question| &question.options)
                .any(|option| option.label.to_lowercase() == text.to_lowercase())
    }

    pub fn matches_visible(&self, visible: &str) -> bool {
        if !self.supported() {
            return false;
        }
        let question = &self.questions[0];
        let mut expected = question.question.clone();
        for (index, option) in question.options.iter().enumerate() {
            expected.push_str(&format!(
                "{}.{}{}",
                index + 1,
                option.label,
                option.description
            ));
        }
        if question.custom {
            expected.push_str(&format!(
                "{}.Type your own answer",
                question.options.len() + 1
            ));
        }
        expected.push_str("↑↓ select enter submit esc dismiss");
        compact(&main_column(visible)).ends_with(&compact(&expected))
    }
}

fn main_column(visible: &str) -> String {
    // OpenCode's default wide sidebar has two padding cells before its version footer.
    // Slice by display cells, not UTF-8 offsets, so wrapped Unicode options still match.
    let boundary = visible
        .lines()
        .rev()
        .find_map(|line| {
            let prefix = &line[..line.rfind("• OpenCode ")?];
            if !prefix.ends_with("  ") {
                return None;
            }
            UnicodeWidthStr::width(prefix)
                .checked_sub(2)
                .filter(|column| *column >= 20)
        })
        .or_else(|| {
            // V2 can replace the version footer with the repository path. Accept
            // only the bottom gutter-only row, not a path in transcript content.
            let line = visible.lines().rev().find(|line| !line.trim().is_empty())?;
            let tail = line.trim_start().strip_prefix('┃')?;
            let location = tail.trim_start();
            if !location.starts_with("~/") && !location.starts_with('/') {
                return None;
            }
            let prefix = &line[..line.len() - location.len()];
            UnicodeWidthStr::width(prefix)
                .checked_sub(2)
                .filter(|column| *column >= 20)
        });
    let Some(boundary) = boundary else {
        return visible.into();
    };
    visible
        .lines()
        .map(|line| {
            if UnicodeWidthStr::width(line) <= boundary {
                return line.to_string();
            }
            // Whole-prefix widths preserve joined emoji while trimming the small sidebar.
            let end = line
                .char_indices()
                .rev()
                .find_map(|(end, _)| {
                    (UnicodeWidthStr::width(&line[..end]) <= boundary).then_some(end)
                })
                .unwrap_or(0);
            line[..end].to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '│' | '┃' | '║' | '|'))
        .collect()
}

pub fn from_export(value: &Value) -> Result<Option<Decision>> {
    from_data(&serde_json::from_value(value.clone())?)
}

pub(super) fn from_data(export: &super::Export) -> Result<Option<Decision>> {
    let latest_user = export
        .messages
        .iter()
        .rposition(|message| message.info.role == "user");
    let mut found = None;
    for (index, message) in export.messages.iter().enumerate() {
        if latest_user.is_some_and(|user| index < user)
            || message.info.role != "assistant"
            || message.info.error.is_some()
            || message.info.summary == true
        {
            continue;
        }
        for part in &message.parts {
            if part.kind != "tool"
                || part.tool.as_deref() != Some("question")
                || part.state["status"] != "running"
            {
                continue;
            }
            let questions: Vec<Question> =
                serde_json::from_value(part.state["input"]["questions"].clone())?;
            ensure!(
                !questions.is_empty() && questions.len() <= 8,
                "Invalid pending question count"
            );
            let mut size = 0;
            for question in &questions {
                ensure!(
                    !question.question.trim().is_empty()
                        && question.question.len() <= 4000
                        && question.header.len() <= 200
                        && !question.options.is_empty()
                        && question.options.len() <= 16,
                    "Invalid pending question"
                );
                size += question.question.len() + question.header.len();
                for option in &question.options {
                    ensure!(
                        !option.label.trim().is_empty()
                            && option.label.len() <= 200
                            && option.description.len() <= 1000,
                        "Invalid question option"
                    );
                    size += option.label.len() + option.description.len();
                }
            }
            ensure!(size <= 12000, "Pending question is too large");
            let id = part
                .id
                .clone()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Pending question has no identity"))?;
            ensure!(found.is_none(), "Multiple pending questions are ambiguous");
            found = Some(Decision {
                id: format!("{}:{id}", message.info.id),
                form_id: None,
                questions,
                announced: false,
                submitted: false,
                requires_token: false,
            });
        }
    }
    Ok(found)
}

pub async fn read(
    config: &crate::BridgeConfig,
    session: &crate::herdr::Session,
) -> Result<Option<Decision>> {
    if session.agent != "opencode" || session.kind != "id" {
        return Ok(None);
    }
    Ok(
        super::read_opencode_snapshot(config, session, Default::default())
            .await?
            .decision,
    )
}

/// V2 exports contain settled messages only; pending questions live in session forms.
pub(super) async fn read_v2(bin: &std::path::Path, session: &str) -> Result<Option<Decision>> {
    ensure!(
        session.starts_with("ses_")
            && session
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        "Invalid OpenCode session identity"
    );
    let path = format!("/api/session/{session}/form");
    let output = super::run_command(bin, &["api", "get", &path], 1024 * 1024).await?;
    let value: Value = serde_json::from_reader(std::io::BufReader::new(output))?;
    from_forms(&value, session)
}

pub fn from_forms(value: &Value, session: &str) -> Result<Option<Decision>> {
    let forms = value["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid session forms"))?;
    let mut found = None;
    for form in forms
        .iter()
        .filter(|form| form["metadata"]["kind"] == "question")
    {
        ensure!(
            form["sessionID"] == session,
            "Question belongs to another session"
        );
        ensure!(found.is_none(), "Multiple pending questions are ambiguous");
        let message = form["metadata"]["tool"]["messageID"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Question has no message identity"))?;
        let tool = form["metadata"]["tool"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Question has no tool identity"))?;
        let fields = form["fields"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid question fields"))?;
        let mut questions = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            ensure!(
                field["key"] == format!("q{index}")
                    && matches!(field["type"].as_str(), Some("string" | "multiselect")),
                "Unsupported question field"
            );
            let options = field["options"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Question has no options"))?;
            ensure!(options.iter().all(|option| option["value"].is_string() && option["value"] == option["label"]),
                "Unsupported question option mapping");
            questions.push(serde_json::json!({
                "question": field["description"], "header": field["title"], "options": options,
                "custom": field.get("custom").cloned().unwrap_or(Value::Bool(false)),
                "multiple": field["type"] == "multiselect"
            }));
        }
        // Share the legacy parser's size, option-count, and ambiguity checks.
        found = from_export(&serde_json::json!({"messages":[{
            "info":{"id":message,"role":"assistant"}, "parts":[{
                "type":"tool","id":tool,"tool":"question",
                "state":{"status":"running","input":{"questions":questions}}
            }]
        }]}))?;
        if let Some(id) = form.get("id") {
            let id = id
                .as_str()
                .filter(|id| {
                    id.starts_with("frm_")
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                })
                .ok_or_else(|| anyhow::anyhow!("Invalid question form identity"))?;
            if let Some(decision) = found.as_mut() {
                decision.form_id = Some(id.into());
            }
        }
    }
    Ok(found)
}
