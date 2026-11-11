//! Bounded, read-only timeline collection. No cursor can skip older late arrivals.
use crate::wire::{PROTOCOL, Socket};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::{
    sync::{Notify, mpsc},
    task::JoinHandle,
    time::Instant,
};

pub(crate) struct Group {
    pub group: String,
    pub seen: HashSet<String>,
}

#[derive(Default)]
pub(crate) struct Recovery {
    task: Option<JoinHandle<Result<()>>>,
    events: Option<mpsc::Receiver<Value>>,
    requested: bool,
    retry_at: Option<Instant>,
    failures: u32,
}

impl Drop for Recovery {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Recovery {
    pub fn request(&mut self) {
        self.requested = true;
    }

    pub fn due(&self) -> bool {
        self.task.is_none() && self.requested && self.retry_at.is_none_or(|at| Instant::now() >= at)
    }

    pub fn pop(&mut self) -> Option<Value> {
        self.events.as_mut()?.try_recv().ok()
    }

    pub fn failed(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.events = None;
        self.failures = self.failures.saturating_add(1);
        self.retry_at =
            Some(Instant::now() + Duration::from_secs((1_u64 << self.failures.min(6)).min(60)));
        self.requested = true;
    }

    pub async fn finish(&mut self) {
        if self.task.as_ref().is_some_and(|task| task.is_finished())
            && self.events.as_ref().is_none_or(|events| events.is_empty())
        {
            let result = self.task.take().unwrap().await;
            self.events = None;
            if matches!(result, Ok(Ok(()))) {
                self.failures = 0;
                self.retry_at = None;
            } else {
                self.failed();
            }
        }
    }

    pub fn start(
        &mut self,
        socket: Socket,
        account: String,
        phone: String,
        groups: Vec<Group>,
        notify: Arc<Notify>,
    ) {
        if !self.due() {
            return;
        }
        self.requested = false;
        let (tx, rx) = mpsc::channel(8);
        self.events = Some(rx);
        self.task = Some(tokio::spawn(async move {
            let mut failed = false;
            for group in groups {
                if stream_group(&socket, &account, &phone, &group, &tx, &notify)
                    .await
                    .is_err()
                {
                    failed = true;
                }
            }
            notify.notify_one();
            ensure!(!failed, "Some groups still need inbound recovery");
            Ok(())
        }));
    }
}

pub(crate) async fn stream_group(
    socket: &Socket,
    account: &str,
    phone: &str,
    group: &Group,
    tx: &mpsc::Sender<Value>,
    notify: &Notify,
) -> Result<()> {
    let mut before: Option<(u64, String)> = None;
    let mut cursors = HashSet::new();
    let mut pending = false;
    let mut traversed = false;
    // Find the oldest page on every pass. Retain only one page, not a payload
    // backlog or a persisted timestamp watermark that misses late old rows.
    let mut page = loop {
        let mut request = json!({"type":"timeline_list","account_id_hex":account,"group_id_hex":group.group,"limit":50});
        if let Some(cursor) = &before {
            request["before"] = json!({"recorded_at":cursor.0,"message_id_hex":cursor.1});
        }
        let page = socket.marmot(request).await?;
        ensure!(page["type"] == "timeline_page", "Invalid timeline response");
        let rows = page["messages"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing timeline messages"))?;
        if rows.is_empty() {
            return Ok(());
        }
        let cursor = cursor(&rows[0])?;
        ensure!(
            cursors.insert(cursor.1.clone()),
            "Timeline cursor did not advance"
        );
        pending |= rows.iter().any(|row| eligible(row, phone, group));
        if page["has_more_before"] != true {
            break page;
        }
        before = Some(cursor);
        traversed = true;
    };
    if !pending {
        return Ok(());
    }
    // MDK orders group rows by authenticated epoch, not wall-clock time.
    // Retain cursor identities for cycle detection, never compare timestamps.
    cursors.clear();
    let mut after = None;
    loop {
        let rows = page["messages"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing timeline messages"))?;
        if rows.is_empty() {
            return Ok(());
        }
        let next = cursor(rows.last().unwrap())?;
        ensure!(
            cursors.insert(next.1.clone()),
            "Timeline cursor did not advance"
        );
        for row in rows.iter().filter(|row| eligible(row, phone, group)) {
            tx.send(event(account, &group.group, row.clone()))
                .await
                .map_err(|_| anyhow::anyhow!("Recovery consumer closed"))?;
            notify.notify_one();
        }
        if !traversed || (after.is_some() && page["has_more_after"] != true) {
            return Ok(());
        }
        // This forward cursor is scoped to this full scan only. The next scan
        // starts backwards again and can discover newly ingested older rows.
        page = socket
            .marmot(
                json!({"type":"timeline_list","account_id_hex":account,"group_id_hex":group.group,
            "after":{"recorded_at":next.0,"message_id_hex":next.1},"limit":50}),
            )
            .await?;
        ensure!(page["type"] == "timeline_page", "Invalid timeline response");
        after = Some(next);
    }
}

fn cursor(row: &Value) -> Result<(u64, String)> {
    Ok((
        row["recorded_at"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid timeline time"))?,
        row["message_id_hex"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing timeline message ID"))?
            .to_owned(),
    ))
}

fn eligible(row: &Value, phone: &str, group: &Group) -> bool {
    row["availability"] == "available"
        && row["kind"] == 9
        && row["sender"]["account_id_hex"] == phone
        && row["sender"]["is_self"] != true
        && row["message_id_hex"]
            .as_str()
            .is_some_and(|id| !group.seen.contains(id))
}

pub(crate) fn event(account: &str, group: &str, row: Value) -> Value {
    json!({"marmot_agent_control":PROTOCOL,"type":"inbound_message","account_id_hex":account,"group_id_hex":group,
        "reply_to":{"message_id_hex":row["reply_to_message_id_hex"]},"message":{"message_id_hex":row["message_id_hex"],
        "sender":row["sender"],"text":row["text"],"text_truncated":row["text_truncated"],"attachments_truncated":row["attachments_truncated"],
        "recorded_at":row["recorded_at"],"media":row["attachments"]}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        net::UnixListener,
    };

    #[tokio::test]
    async fn recovery_stream_obeys_backpressure_and_keeps_oldest_first() {
        check_stream(false).await;
    }

    #[tokio::test]
    async fn recovery_stream_follows_canonical_identity_when_epoch_timestamps_decrease() {
        check_stream(true).await;
    }

    async fn check_stream(decreasing: bool) {
        let root = tempfile::tempdir().unwrap();
        let socket = Socket {
            path: root.path().join("timeline.sock"),
        };
        let listener = UnixListener::bind(&socket.path).unwrap();
        let forwards = Arc::new(AtomicUsize::new(0));
        let count = forwards.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let request = crate::wire::read_line(&mut reader).await.unwrap();
                let (start, end) = if let Some(cursor) = request.get("after") {
                    count.fetch_add(1, Ordering::SeqCst);
                    let start =
                        usize::from_str_radix(cursor["message_id_hex"].as_str().unwrap(), 16)
                            .unwrap()
                            + 1;
                    (start, (start + 50).min(100))
                } else {
                    let end = request.get("before").map_or(100, |cursor| {
                        usize::from_str_radix(cursor["message_id_hex"].as_str().unwrap(), 16)
                            .unwrap()
                    });
                    (end.saturating_sub(50), end)
                };
                let rows: Vec<_> = (start..end).map(|n| json!({"message_id_hex":format!("{n:064x}"),"recorded_at":if decreasing && n < 50 {1000+n} else {n},"availability":"available","kind":9,"sender":{"account_id_hex":"owner"},"text":"fixture"})).collect();
                let page = json!({"id":request["id"],"marmot_agent_control":PROTOCOL,"type":"timeline_page","messages":rows,"has_more_before":start>0,"has_more_after":end<100});
                reader
                    .get_mut()
                    .write_all(format!("{page}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let (tx, mut rx) = mpsc::channel(8);
        let worker = tokio::spawn(async move {
            stream_group(
                &socket,
                "account",
                "owner",
                &Group {
                    group: "group".into(),
                    seen: HashSet::new(),
                },
                &tx,
                &Notify::new(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while rx.len() < 8 {
                if worker.is_finished() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            forwards.load(Ordering::SeqCst),
            0,
            "Do not fetch another payload page while the event channel is full"
        );
        let mut times = Vec::new();
        while let Some(event) = rx.recv().await {
            times.push(event["message"]["recorded_at"].as_u64().unwrap());
        }
        worker.await.unwrap().unwrap();
        assert_eq!(
            times,
            (0..100)
                .map(|n| if decreasing && n < 50 { 1000 + n } else { n })
                .collect::<Vec<_>>()
        );
        server.abort();
    }
}
