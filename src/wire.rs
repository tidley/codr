use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

pub const PROTOCOL: &str = "marmot.agent-control.v2";
pub const MAX_FRAME: usize = 1024 * 1024;

pub(crate) fn is_chat_timeline_row(row: &Value) -> bool {
    // MDK 0.19 projects a thread's authoritative lowercase `e` parent here.
    // Other kind-1111/custom rows must not become agent prompts or chat activity.
    row["kind"] == 9 || (row["kind"] == 1111 && row["reply_to_message_id_hex"].is_string())
}

#[derive(Debug, thiserror::Error)]
#[error("Socket unavailable before request submission")]
pub struct NotSubmitted;

#[derive(Debug, thiserror::Error)]
#[error("Remote method not found")]
pub struct MethodNotFound;

#[derive(Debug, thiserror::Error)]
#[error("Remote request envelope rejected")]
pub struct InvalidRequest;

pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value> {
    let mut bytes = vec![];
    let size = reader
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        size > 0 && size <= MAX_FRAME && bytes.last() == Some(&b'\n'),
        "Invalid or oversized socket frame"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Clone)]
pub struct Socket {
    pub path: PathBuf,
}

impl Socket {
    pub async fn exchange(&self, mut value: Value, marmot: bool) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        value["id"] = json!(id);
        if marmot {
            value["marmot_agent_control"] = json!(PROTOCOL);
        }
        let mut stream =
            match timeout(Duration::from_secs(10), UnixStream::connect(&self.path)).await {
                Ok(Ok(stream)) => stream,
                _ => return Err(NotSubmitted.into()),
            };
        timeout(Duration::from_secs(90), async {
            let mut bytes = serde_json::to_vec(&value)?;
            bytes.push(b'\n');
            stream.write_all(&bytes).await?;
            let response = read_line(&mut BufReader::new(stream)).await?;
            ensure!(response["id"] == id, "Socket response ID mismatch");
            if marmot {
                ensure!(
                    response["marmot_agent_control"] == PROTOCOL,
                    "MDK protocol mismatch"
                );
            }
            if response.get("error").is_some() || response["type"] == "error" {
                // Typed protocol errors permit backend fallback. An ambiguous
                // reply (even result:null) is not evidence for that decision.
                ensure!(
                    response.get("result").is_none(),
                    "Malformed socket reply: both result and error"
                );
                if !marmot && matches!(response["error"]["code"].as_i64(), Some(-32601 | -32600)) {
                    // Old TUI replies omit jsonrpc. An explicit wrong version or
                    // incomplete error is not a capability proof, even if a later
                    // snapshot would succeed; never use it to enable creation.
                    ensure!(
                        response.get("jsonrpc").is_none() || response["jsonrpc"] == "2.0",
                        "Malformed protocol error version"
                    );
                    ensure!(
                        response["error"]["message"].is_string(),
                        "Malformed protocol error"
                    );
                    if response["error"]["code"] == -32601 {
                        return Err(MethodNotFound.into());
                    }
                    return Err(InvalidRequest.into());
                }
                // Remote messages can contain private data. Expose only the bounded error code.
                let raw = response["error"]["code"]
                    .as_str()
                    .or_else(|| response["code"].as_str())
                    .unwrap_or("remote_error");
                let code: String = raw
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(80)
                    .collect();
                bail!("Remote request failed: {code}");
            }
            Ok(response)
        })
        .await
        .map_err(|_| anyhow::anyhow!("Socket request timed out; result may be unknown"))?
    }

    pub async fn herdr(&self, method: &str, params: Value) -> Result<Value> {
        self.exchange(json!({"method":method,"params":params}), false)
            .await
    }
    pub async fn core(&self, method: &str, params: Value) -> Result<Value> {
        // Keep the old TUI envelope unchanged in herdr(); only Core calls
        // require the JSON-RPC marker on both sides.
        let response = self
            .exchange(
                json!({"jsonrpc":"2.0","method":method,"params":params}),
                false,
            )
            .await?;
        ensure!(
            response["jsonrpc"] == "2.0" && response.get("result").is_some(),
            "Malformed Core JSON-RPC reply"
        );
        Ok(response["result"].clone())
    }
    pub async fn marmot(&self, value: Value) -> Result<Value> {
        self.exchange(value, true).await
    }
    pub async fn subscribe(
        &self,
        mut request: Value,
        marmot: bool,
    ) -> Result<BufReader<UnixStream>> {
        let id = uuid::Uuid::new_v4().to_string();
        request["id"] = json!(id);
        if marmot {
            request["marmot_agent_control"] = json!(PROTOCOL);
        }
        let mut stream =
            timeout(Duration::from_secs(10), UnixStream::connect(&self.path)).await??;
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let mut reader = BufReader::new(stream);
        let ack = timeout(Duration::from_secs(10), read_line(&mut reader)).await??;
        ensure!(ack["id"] == id, "Subscription ID mismatch");
        if marmot {
            ensure!(
                ack["type"] == "ack" && ack["marmot_agent_control"] == PROTOCOL,
                "MDK subscription rejected"
            );
        } else {
            ensure!(
                ack["result"]["type"] == "subscription_started",
                "herdr subscription rejected"
            );
        }
        Ok(reader)
    }
}
