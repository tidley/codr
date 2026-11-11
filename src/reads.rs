use crate::{BridgeConfig, herdr::Agent, output};
use anyhow::Result;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

#[derive(Debug, thiserror::Error)]
#[error("Session read pending")]
pub(crate) struct Pending;

#[derive(Clone, PartialEq)]
struct Context {
    sequence: u64,
    status: String,
    revision: Option<String>,
    ended: bool,
}

struct Job {
    context: Context,
    task: JoinHandle<Result<output::SessionSnapshot>>,
}

/// Shared native reader and I/O budget for polling and explicit history reads.
#[derive(Clone)]
pub(crate) struct Reader {
    limit: Arc<Semaphore>,
    database: Arc<Mutex<output::opencode::Reader>>,
    service: Arc<output::service::Reader>,
}

impl Reader {
    pub async fn read(
        &self,
        config: &BridgeConfig,
        session: &crate::herdr::Session,
    ) -> Result<output::SessionSnapshot> {
        let _permit = self.limit.acquire().await?;
        self.validate(config, session).await
    }

    async fn validate(
        &self,
        config: &BridgeConfig,
        session: &crate::herdr::Session,
    ) -> Result<output::SessionSnapshot> {
        if session.agent == "opencode" && config.opencode_service_file.is_some() {
            anyhow::ensure!(
                session.kind == "id",
                "Unsupported native session reference kind"
            );
            self.service.read(config, &session.value).await
        } else {
            snapshot(config, session, self.database.clone()).await
        }
    }
}

/// One reader per session, with bounded parallel I/O and serial result application.
pub(crate) struct Reads {
    pub background: bool,
    pub scheduling: bool,
    pub notify: Arc<Notify>,
    limit: Arc<Semaphore>,
    jobs: BTreeMap<String, Job>,
    ready: BTreeMap<String, (Context, Arc<output::SessionSnapshot>)>,
    ended: HashSet<String>,
    pub database: Arc<Mutex<output::opencode::Reader>>,
    service: Arc<output::service::Reader>,
    completed: Arc<Mutex<HashSet<String>>>,
}

impl Default for Reads {
    fn default() -> Self {
        Self {
            background: false,
            scheduling: true,
            notify: Arc::new(Notify::new()),
            limit: Arc::new(Semaphore::new(4)),
            jobs: BTreeMap::new(),
            ready: BTreeMap::new(),
            ended: HashSet::new(),
            database: Arc::new(Mutex::new(output::opencode::Reader::default())),
            service: Arc::new(output::service::Reader::default()),
            completed: Arc::default(),
        }
    }
}

impl Drop for Reads {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.task.abort();
        }
    }
}

impl Reads {
    pub fn reader(&self) -> Reader {
        Reader {
            limit: self.limit.clone(),
            database: self.database.clone(),
            service: self.service.clone(),
        }
    }
    pub fn completed_keys(&self) -> Vec<String> {
        let mut completed = self.completed.lock().unwrap();
        let keys: Vec<_> = completed.drain().collect();
        let mut ready = Vec::new();
        for key in keys {
            if let Some(job) = self.jobs.get(&key) {
                if job.task.is_finished() {
                    ready.push(key);
                } else {
                    // Notification can race with the task returning its result.
                    completed.insert(key);
                    self.notify.notify_one();
                }
            }
        }
        ready
    }

    pub fn retain(&mut self, keys: &HashSet<String>) {
        self.ready.retain(|key, _| keys.contains(key));
        // Let in-flight native readers finish before releasing their concurrency slot.
        self.jobs
            .retain(|key, job| keys.contains(key) || !job.task.is_finished());
        self.ended.retain(|key| keys.contains(key));
        let sessions = keys
            .iter()
            .filter_map(|key| serde_json::from_str::<Vec<String>>(key).ok())
            .filter(|parts| parts.len() == 4 && parts[1] == "opencode")
            .map(|parts| parts[3].clone())
            .collect();
        self.service.retain(&sessions);
        if let Ok(mut reader) = self.database.try_lock() {
            let sessions = keys
                .iter()
                .filter_map(|key| serde_json::from_str::<Vec<String>>(key).ok())
                .filter(|parts| parts.len() == 4 && parts[1] == "opencode")
                .map(|parts| parts[3].clone())
                .collect();
            reader.retain(&sessions);
        }
    }

    pub fn set_ended(&mut self, key: &str, ended: bool) {
        if ended {
            self.ended.insert(key.to_owned());
        } else {
            self.ended.remove(key);
        }
    }

    pub fn finish(&mut self, key: &str) {
        self.ready.remove(key);
    }

    pub async fn fresh(
        &self,
        config: &BridgeConfig,
        session: &crate::herdr::Session,
    ) -> Result<output::SessionSnapshot> {
        // Foreground validation is awaited by serial dispatch. Preserve its
        // independent path so unrelated background jobs cannot queue controls.
        self.reader().validate(config, session).await
    }

    pub async fn read(
        &mut self,
        config: &BridgeConfig,
        agent: &Agent,
        revision: Option<String>,
    ) -> Result<Arc<output::SessionSnapshot>> {
        let key = agent.key();
        let context = Context {
            sequence: agent.state_change_seq,
            status: agent.agent_status.clone(),
            revision,
            ended: self.ended.contains(&key),
        };
        if let Some((old, snapshot)) = self.ready.get(&key)
            && old == &context
        {
            return Ok(snapshot.clone());
        }
        self.ready.remove(&key);
        if self
            .jobs
            .get(&key)
            .is_some_and(|job| job.task.is_finished())
        {
            let job = self.jobs.remove(&key).unwrap();
            let result = job
                .task
                .await
                .map_err(|_| anyhow::anyhow!("Session reader failed"))?;
            if job.context == context {
                let snapshot = Arc::new(result.inspect_err(|error| {
                    eprintln!(
                        "Session history read failed: {}.",
                        output::read_error_message(error)
                    );
                })?);
                self.ready
                    .insert(key.clone(), (context.clone(), snapshot.clone()));
                return Ok(snapshot);
            }
            // Finish stale readers before replacing them, including spawn_blocking reads.
        }
        let session = agent
            .agent_session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Session identity unavailable"))?
            .clone();
        if !self.background {
            let snapshot = Arc::new(self.fresh(config, &session).await?);
            self.ready.insert(key, (context, snapshot.clone()));
            return Ok(snapshot);
        }
        if !self.scheduling {
            return Err(Pending.into());
        }
        let reader = self.reader();
        self.jobs.entry(key.clone()).or_insert_with(|| {
            let config = config.clone();
            let notify = self.notify.clone();
            let completed = self.completed.clone();
            Job {
                context,
                task: tokio::spawn(async move {
                    let result = reader.read(&config, &session).await;
                    completed.lock().unwrap().insert(key);
                    notify.notify_one();
                    result
                }),
            }
        });
        Err(Pending.into())
    }
}

pub(crate) async fn snapshot(
    config: &BridgeConfig,
    session: &crate::herdr::Session,
    database: Arc<Mutex<output::opencode::Reader>>,
) -> Result<output::SessionSnapshot> {
    if config.opencode_db.is_some() && session.agent == "opencode" {
        anyhow::ensure!(
            session.kind == "id",
            "Unsupported native session reference kind"
        );
        return output::read_opencode_snapshot(config, session, database).await;
    }
    output::read_snapshot(config, session).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bech32::ToBase32;
    use serde_json::json;

    #[tokio::test]
    async fn fresh_validation_does_not_queue_behind_background_capacity() {
        let root = tempfile::tempdir().unwrap();
        let reads = Reads::default();
        let _busy = reads.limit.acquire_many(4).await.unwrap();
        let config: BridgeConfig = serde_json::from_value(json!({
            "phone_npub":bech32::encode("npub",[0x11u8;32].to_base32(),bech32::Variant::Bech32).unwrap(),
            "account_id_hex":"aa".repeat(32), "herdr_socket":root.path().join("h.sock"),
            "wn_agent_socket":root.path().join("w.sock"), "state_path":root.path().join("state.json"),
            "opencode_service_file":root.path().join("absent-registration.json")
        })).unwrap();
        let session = crate::herdr::Session {
            source: "fixture".into(),
            agent: "opencode".into(),
            kind: "id".into(),
            value: "ses_fixture".into(),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            reads.fresh(&config, &session),
        )
        .await;
        assert!(
            result.is_ok(),
            "Fresh validation must reach its reader despite saturated background work"
        );
        assert!(
            result.unwrap().is_err(),
            "The synthetic registration is intentionally absent"
        );
    }
}
