//! Read-only manual history collection. Results are applied by the serial bridge.
use crate::{BridgeConfig, herdr::Session, output::SessionHistory, reads::Reader, wire::Socket};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub key: String,
    pub group: String,
    pub session: Session,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    Recent(u64),
    Last(usize),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub key: String,
    pub group: String,
    pub id: String,
    pub kind: Kind,
    pub requested_at: u64,
    pub targets: Vec<Target>,
    pub reference: Option<u64>,
    pub groups: Vec<String>,
}

pub(crate) struct Collection {
    pub request: Request,
    pub histories: Vec<(Target, Result<std::sync::Arc<SessionHistory>>)>,
    pub reference: Option<u64>,
    pub unavailable: usize,
}

pub(crate) async fn collect(
    reader: Reader,
    config: BridgeConfig,
    marmot: Socket,
    request: Request,
) -> Collection {
    let mut reference = request.reference;
    let mut unavailable = 0;
    if matches!(request.kind, Kind::Recent(_)) && reference.is_none() {
        for group in &request.groups {
            match last_own(&marmot, &config.account_id_hex, group, request.requested_at).await {
                Ok(at) => reference = reference.max(at),
                Err(_) => unavailable += 1,
            }
        }
    }
    let mut jobs = tokio::task::JoinSet::new();
    for target in request.targets.clone() {
        let reader = reader.clone();
        let config = config.clone();
        jobs.spawn(async move {
            let history = reader
                .read(&config, &target.session)
                .await
                .map(|snapshot| snapshot.history);
            (target, history)
        });
    }
    let mut histories = Vec::new();
    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(result) => histories.push(result),
            Err(_) => unavailable += 1,
        }
    }
    Collection {
        request,
        histories,
        reference,
        unavailable,
    }
}

async fn last_own(
    marmot: &Socket,
    account: &str,
    group: &str,
    requested_at: u64,
) -> Result<Option<u64>> {
    let mut before: Option<serde_json::Value> = None;
    let mut cursors = HashSet::new();
    let mut latest = None;
    loop {
        let mut request = json!({"type":"timeline_list","account_id_hex":account,"group_id_hex":group,"limit":50});
        if let Some(cursor) = &before {
            request["before"] = cursor.clone();
        }
        let page = marmot.marmot(request).await?;
        ensure!(page["type"] == "timeline_page", "Invalid timeline response");
        let rows = page["messages"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing timeline messages"))?;
        latest = latest.max(
            rows.iter()
                .filter(|row| {
                    row["sender"]["is_self"] == true
                        && row["kind"] == 9
                        && row["availability"] == "available"
                })
                .filter_map(|row| row["recorded_at"].as_u64())
                .filter(|at| *at > 0 && *at <= requested_at)
                .max(),
        );
        if rows.is_empty() || page["has_more_before"] != true {
            return Ok(latest);
        }
        let at = rows[0]["recorded_at"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid timeline time"))?;
        let id = rows[0]["message_id_hex"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing timeline message ID"))?;
        ensure!(
            cursors.insert(id.to_owned()),
            "Timeline cursor did not advance"
        );
        before = Some(json!({"recorded_at":at,"message_id_hex":id}));
    }
}
