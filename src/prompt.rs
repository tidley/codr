//! OpenCode V2 admission. Never starts a service, retries a POST, or types a fallback.
use crate::{BridgeConfig, activity::UserInput, herdr::Agent, output::service};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

pub fn native_session<'a>(config: &BridgeConfig, agent: &'a Agent) -> Option<&'a str> {
    agent
        .agent_session
        .as_ref()
        .filter(|session| {
            config.opencode_service_file.is_some()
                && agent.agent == "opencode"
                && session.agent == "opencode"
                && session.kind == "id"
        })
        .map(|session| session.value.as_str())
}

struct Client {
    http: reqwest::Client,
    registration: service::Registration,
}

impl Client {
    async fn new(config: &BridgeConfig, session: &str, id: &str) -> Result<Self> {
        ensure!(
            safe_id(session, "ses_") && safe_id(id, "msg_"),
            "Invalid native prompt identity"
        );
        let registration = service::registration(config).await?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self { http, registration })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self.http.request(
            method,
            format!("{}{path}", self.registration.url.trim_end_matches('/')),
        );
        match &self.registration.password {
            Some(password) => request.basic_auth("opencode", Some(password)),
            None => request,
        }
    }
}

fn safe_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

async fn body(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= 1024 * 1024,
            "Native prompt response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn inbox_input(value: &Value, session: &str, id: &str) -> Option<UserInput> {
    (value["type"] == "user" && value["id"] == id && value["sessionID"] == session)
        .then(|| value["payload"]["text"].as_str())
        .flatten()
        .map(|text| UserInput::new(format!("opencode:{id}"), text))
}

pub async fn send(config: &BridgeConfig, session: &str, id: &str, text: &str) -> Result<UserInput> {
    let reservation =
        crate::native_rooms::guard(config, ["opencode".into(), "id".into(), session.into()])
            .map_err(|_| anyhow::Error::new(crate::wire::NotSubmitted))?;
    reservation
        .allow(config)
        .map_err(|_| anyhow::Error::new(crate::wire::NotSubmitted))?;
    let result = send_reserved(config, session, id, text).await;
    drop(reservation);
    result
}

/// Room callers already hold the matching native guard through their durable
/// policy/submission update. This seam must not be exposed as a host operation.
pub(crate) async fn send_reserved(
    config: &BridgeConfig,
    session: &str,
    id: &str,
    text: &str,
) -> Result<UserInput> {
    // Only setup errors are proven pre-submission. Once send() starts, every
    // failure remains uncertain and must be resolved by readback, never retry.
    let client = Client::new(config, session, id)
        .await
        .map_err(|_| anyhow::Error::new(crate::wire::NotSubmitted))?;
    let response = client
        .request(
            reqwest::Method::POST,
            &format!("/api/session/{session}/prompt"),
        )
        .json(&json!({"id":id,"text":text,"delivery":"steer"}))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("OpenCode prompt admission unconfirmed"))?;
    ensure!(
        response.status().is_success(),
        "OpenCode prompt admission unconfirmed"
    );
    let value = body(response)
        .await
        .map_err(|_| anyhow::anyhow!("OpenCode prompt receipt unavailable"))?;
    ensure!(
        value["data"]["payload"]["text"] == text,
        "OpenCode prompt receipt differs"
    );
    inbox_input(&value["data"], session, id)
        .ok_or_else(|| anyhow::anyhow!("OpenCode prompt receipt differs"))
}

/// Read-only reconciliation of a retained admission, including after restart.
pub async fn accepted(config: &BridgeConfig, session: &str, id: &str) -> Result<Option<UserInput>> {
    let client = Client::new(config, session, id).await?;
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/api/session/{session}/message/{id}"),
        )
        .send()
        .await?;
    if response.status().is_success() {
        let value = body(response).await?;
        let message = &value["data"];
        ensure!(
            message["type"] == "user" && message["id"] == id,
            "Native message identity differs"
        );
        return Ok(message["text"]
            .as_str()
            .map(|text| UserInput::new(format!("opencode:{id}"), text)));
    }
    ensure!(
        response.status() == reqwest::StatusCode::NOT_FOUND,
        "Native message read unavailable"
    );
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/api/session/{session}/inbox"),
        )
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Native inbox read unavailable"
    );
    let value = body(response).await?;
    let items = value["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid native inbox"))?;
    Ok(items.iter().find_map(|item| inbox_input(item, session, id)))
}
