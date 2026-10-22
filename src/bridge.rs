use crate::{
    BridgeConfig, BridgeState,
    config::phone_hex,
    forwarding,
    herdr::{self, Agent},
    output,
    state::Mapping,
    wire::{PROTOCOL, Socket, read_line},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Bridge {
    pub config: BridgeConfig,
    pub state: BridgeState,
    pub agents: BTreeMap<String, Agent>,
    pub herdr: Socket,
    pub marmot: Socket,
    pub herdr_available: bool,
    phone: String,
    media_job: Option<tokio::task::JoinHandle<Result<crate::media::Prepared>>>,
    reads: crate::reads::Reads,
    last_snapshot: Vec<Agent>,
    reconcile_errors: BTreeMap<String, String>,
}

/// Log a session failure once per distinct error instead of on every three-second snapshot.
fn reconcile_warning(
    errors: &mut BTreeMap<String, String>,
    key: &str,
    error: &anyhow::Error,
) -> Option<String> {
    let detail = format!("{error:#}");
    if errors.get(key) == Some(&detail) {
        return None;
    }
    errors.insert(key.to_owned(), detail.clone());
    Some(format!("Session {key} could not reconcile: {detail}"))
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(job) = self.media_job.take() {
            job.abort();
        }
    }
}

impl Bridge {
    pub fn open(config: BridgeConfig) -> Result<Self> {
        let mut state = BridgeState::load(&config.state_path)?;
        let phone = phone_hex(&config.phone_npub)?;
        ensure!(
            state
                .account
                .as_ref()
                .is_none_or(|a| a == &config.account_id_hex)
                && state.phone.as_ref().is_none_or(|p| p == &phone),
            "State belongs to another account or phone"
        );
        state.account = Some(config.account_id_hex.clone());
        state.phone = Some(phone.clone());
        state.save(&config.state_path)?;
        let mut bridge = Self {
            herdr: Socket {
                path: config.herdr_socket.clone(),
            },
            marmot: Socket {
                path: config.wn_agent_socket.clone(),
            },
            config,
            state,
            agents: BTreeMap::new(),
            herdr_available: false,
            phone,
            media_job: None,
            reads: crate::reads::Reads::default(),
            last_snapshot: vec![],
            reconcile_errors: BTreeMap::new(),
        };
        bridge.refresh_forwarding()?;
        Ok(bridge)
    }

    pub fn save(&self) -> Result<()> {
        self.state.save(&self.config.state_path)
    }

    fn refresh_forwarding(&mut self) -> Result<()> {
        let control = forwarding::load(&self.config.state_path)?;
        if self.state.forwarding_revision.as_deref() != Some(&control.revision) {
            // Preserve the old outbox on the first upgrade, unless a mode was already selected.
            let reset = self.state.forwarding_revision.is_some()
                || control.revision != "initial"
                || !control.enabled;
            if reset {
                self.state.outbox.clear();
                for mapping in self.state.mappings.values_mut() {
                    mapping.response_id = None;
                    mapping.task_revision = None;
                    mapping.forwarding_baseline_pending = true;
                    mapping.blocked_context_seq = None;
                    mapping.decision_checked_at = 0;
                    mapping.decision_refresh_pending = true;
                    if let Some(decision) = mapping.decision.as_mut() {
                        decision.announced = false;
                    }
                }
            }
            self.state.forwarding_enabled = control.enabled && !reset;
            self.state.forwarding_resume_pending = control.enabled && reset;
            self.state.forwarding_revision = Some(control.revision);
            self.save()?;
        }
        Ok(())
    }

    async fn activate_forwarding(&mut self) -> Result<()> {
        self.refresh_forwarding()?;
        if !self.state.forwarding_resume_pending || !self.herdr_available {
            return Ok(());
        }
        let revision = self.state.forwarding_revision.clone();
        let keys: Vec<_> = self
            .state
            .mappings
            .iter()
            .filter(|(key, mapping)| {
                mapping.forwarding_baseline_pending
                    && (mapping.active || self.agents.contains_key(*key))
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut agents = Vec::new();
        for key in keys {
            let agent = self.agents.get(&key).cloned().or_else(|| {
                self.state.mappings[&key]
                    .agent_record
                    .clone()
                    .map(|mut agent| {
                        agent.agent_status = "done".into();
                        agent
                    })
            });
            agents.push((
                key,
                agent.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Active session identity is unavailable for its forwarding checkpoint"
                    )
                })?,
            ));
        }
        if self.reads.background {
            // Start every checkpoint before waiting for any one session.
            let mut histories = Vec::new();
            let mut pending = false;
            for (key, agent) in &agents {
                match self.read_history(agent).await {
                    Ok(history) => histories.push((key.clone(), agent.clone(), history)),
                    Err(_) => pending = true,
                }
            }
            for (key, agent, history) in histories {
                self.apply_forwarding_baseline(&key, &agent, Some(history))?;
                self.reads.finish(&key);
            }
            if pending {
                return Err(crate::reads::Pending.into());
            }
            self.refresh_forwarding()?;
            if self.state.forwarding_revision == revision {
                self.state.forwarding_enabled = true;
                self.state.forwarding_resume_pending = false;
                self.save()?;
            }
            return Ok(());
        }
        let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let mut readers = tokio::task::JoinSet::new();
        for (key, agent) in agents {
            let config = self.config.clone();
            let limit = limit.clone();
            let database = self.reads.database.clone();
            readers.spawn(async move {
                let _permit = limit.acquire_owned().await?;
                let history = if let Some(session) = agent
                    .agent_session
                    .as_ref()
                    .filter(|session| output::supported(&session.agent))
                {
                    Some(
                        crate::reads::snapshot(&config, session, database)
                            .await?
                            .history,
                    )
                } else {
                    None
                };
                Ok::<_, anyhow::Error>((key, agent, history))
            });
        }
        let mut failure = None;
        let mut cancelled = false;
        while let Some(result) = readers.join_next().await {
            // Native spawn_blocking readers cannot be aborted. Drain every reader before
            // retrying, so an error or mode change cannot start another four on top.
            if failure.is_some() || cancelled {
                continue;
            }
            let applied = result
                .map_err(|_| anyhow::anyhow!("Forwarding checkpoint task failed"))
                .and_then(|result| result)
                .and_then(|(key, agent, history)| {
                    self.refresh_forwarding()?;
                    if self.state.forwarding_revision != revision {
                        return Ok(false);
                    }
                    self.apply_forwarding_baseline(&key, &agent, history)?;
                    Ok(true)
                });
            match applied {
                Ok(true) => {}
                Ok(false) => cancelled = true,
                Err(error) => failure = Some(error),
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if cancelled {
            return Ok(());
        }
        // A newer Desk selection wins over a slow checkpoint or a recovered phone event.
        self.refresh_forwarding()?;
        if self.state.forwarding_resume_pending && self.state.forwarding_revision == revision {
            self.state.forwarding_enabled = true;
            self.state.forwarding_resume_pending = false;
            self.save()?;
        }
        Ok(())
    }

    async fn baseline_forwarding(&mut self, key: &str, agent: &Agent) -> Result<bool> {
        if !self.state.mappings[key].forwarding_baseline_pending {
            return Ok(false);
        }
        let history = if agent
            .agent_session
            .as_ref()
            .is_some_and(|session| output::supported(&session.agent))
        {
            Some(self.read_history(agent).await?)
        } else {
            None
        };
        self.apply_forwarding_baseline(key, agent, history)?;
        Ok(true)
    }

    fn apply_forwarding_baseline(
        &mut self,
        key: &str,
        agent: &Agent,
        history: Option<output::SessionHistory>,
    ) -> Result<()> {
        if let Some(history) = history {
            let mapping = self.state.mappings.get_mut(key).unwrap();
            let known: HashSet<_> = mapping.final_ids.iter().cloned().collect();
            mapping.final_ids.extend(
                history
                    .finals
                    .into_iter()
                    .map(|reply| reply.id)
                    .filter(|id| !known.contains(id)),
            );
            let muted: HashSet<_> = mapping.muted_turns.iter().cloned().collect();
            mapping
                .muted_turns
                .extend(history.turns.into_iter().filter(|turn| {
                    (agent.settled() || Some(turn) != history.latest_turn.as_ref())
                        && !muted.contains(turn)
                }));
            mapping.output_initialised = true;
            mapping.last_export_at = now();
        }
        let mapping = self.state.mappings.get_mut(key).unwrap();
        mapping.forwarding_baseline_pending = false;
        if agent.settled() {
            mapping.pending_prompt = false;
            mapping.completion_pending = false;
        }
        self.save()?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.refresh_forwarding()?;
        if !self.state.forwarding_enabled {
            return Ok(());
        }
        let mut failed = HashSet::new();
        let mut outbox = self.state.outbox.clone();
        // Durable replies and their continuations precede best-effort progress.
        outbox.sort_by_key(|item| item.response.is_some());
        for item in outbox {
            self.refresh_forwarding()?;
            if !self.state.forwarding_enabled {
                return Ok(());
            }
            if !self
                .state
                .outbox
                .iter()
                .any(|pending| pending.id == item.id)
            {
                continue;
            }
            if failed.contains(&item.group) {
                continue;
            }
            if let Some(delivery) = &item.response {
                if delivery.finished && delivery.target.is_none() && !delivery.durable {
                    self.state.outbox.retain(|m| m.id != item.id);
                    self.save()?;
                    continue;
                }
                if delivery.sent_text.as_deref() == Some(&item.text) {
                    if delivery.finished {
                        self.state.outbox.retain(|m| m.id != item.id);
                        self.save()?;
                    }
                    continue;
                }
                // Persist the attempt before I/O. Never emit competing edits in one second,
                // including after restart or a response lost after publication.
                if now() < delivery.last_attempt_at.saturating_add(2) {
                    continue;
                }
                let pending = self
                    .state
                    .outbox
                    .iter_mut()
                    .find(|m| m.id == item.id)
                    .unwrap()
                    .response
                    .as_mut()
                    .unwrap();
                pending.last_attempt_at = now();
                let initial = pending
                    .initial_text
                    .get_or_insert_with(|| item.text.clone())
                    .clone();
                self.save()?;
                let request = if let Some(target) = &delivery.target {
                    json!({"type":"edit_message","account_id_hex":self.config.account_id_hex,"group_id_hex":item.group,"target_message_id_hex":target,"text":item.text})
                } else {
                    json!({"type":"send_final","account_id_hex":self.config.account_id_hex,"group_id_hex":item.group,"text":initial,"reply_to_message_id_hex":null,"idempotency_key":item.id})
                };
                let result =
                    tokio::time::timeout(Duration::from_secs(3), self.marmot.marmot(request))
                        .await
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Progress update timed out")));
                self.state
                    .outbox
                    .iter_mut()
                    .find(|m| m.id == item.id)
                    .unwrap()
                    .response
                    .as_mut()
                    .unwrap()
                    .last_attempt_at = now();
                self.save()?;
                match result {
                    Ok(response)
                        if response["type"] == "final_sent"
                            && response["message_ids_hex"].as_array().is_some_and(|ids| {
                                !ids.is_empty()
                                    && ids
                                        .iter()
                                        .all(|id| id.as_str().is_some_and(|id| !id.is_empty()))
                            }) =>
                    {
                        let sent_text = if delivery.target.is_some() {
                            item.text.clone()
                        } else {
                            initial
                        };
                        let pending = self
                            .state
                            .outbox
                            .iter_mut()
                            .find(|m| m.id == item.id)
                            .unwrap()
                            .response
                            .as_mut()
                            .unwrap();
                        if pending.target.is_none() {
                            pending.target =
                                response["message_ids_hex"][0].as_str().map(str::to_owned);
                        }
                        pending.sent_text = Some(sent_text.clone());
                        if delivery.finished && sent_text == item.text {
                            self.state.outbox.retain(|m| m.id != item.id);
                        }
                        self.save()?;
                    }
                    _ => {
                        if delivery.finished && !delivery.durable {
                            // A failed terminal progress edit must not hold durable answers.
                            self.state.outbox.retain(|m| m.id != item.id);
                            self.save()?;
                        } else {
                            failed.insert(item.group);
                        }
                    }
                }
                continue;
            }
            match self.marmot.marmot(json!({"type":"send_final","account_id_hex":self.config.account_id_hex,"group_id_hex":item.group,"text":item.text,"reply_to_message_id_hex":null,"idempotency_key":item.id})).await {
                Ok(response) if response["type"] == "final_sent" && response["message_ids_hex"].as_array().is_some_and(|ids|!ids.is_empty()) => {
                    self.state.outbox.retain(|m| m.id != item.id); self.save()?;
                }
                _ => { failed.insert(item.group); }
            }
        }
        ensure!(failed.is_empty(), "Some group deliveries remain pending");
        Ok(())
    }

    pub async fn reconcile(&mut self, agents: Vec<Agent>) -> Result<()> {
        self.refresh_forwarding()?;
        self.last_snapshot = agents.clone();
        let mut current = BTreeMap::new();
        let mut uncertain_terminals = HashSet::new();
        for mut agent in agents {
            // Never attach a group to a location that a replacement session can reuse.
            let Some(session) = agent.agent_session.as_ref() else {
                uncertain_terminals.insert(agent.terminal_id.clone());
                continue;
            };
            if agent.agent.is_empty() {
                agent.agent = session.agent.clone();
                agent.agent_status = "unknown".into();
            }
            ensure!(
                current.insert(agent.key(), agent).is_none(),
                "Two live agents report the same session identity"
            );
        }
        self.agents = current;
        self.reconcile_errors
            .retain(|key, _| self.agents.contains_key(key));
        for key in self.agents.keys() {
            self.reads.set_ended(key, false);
        }
        let read_keys = self
            .agents
            .keys()
            .cloned()
            .chain(
                self.state
                    .mappings
                    .iter()
                    .filter(|(_, mapping)| mapping.active)
                    .map(|(key, _)| key.clone()),
            )
            .collect();
        self.reads.retain(&read_keys);
        self.herdr_available = true;
        self.activate_forwarding().await?;
        ensure!(
            !self.state.forwarding_resume_pending,
            "Forwarding resume awaits its current history checkpoint"
        );
        let keys: Vec<_> = self.agents.keys().cloned().collect();
        for key in keys {
            let agent = self.agents[&key].clone();
            match self.reconcile_agent(&key, &agent).await {
                Ok(()) => {
                    self.reconcile_errors.remove(&key);
                    self.reads.finish(&key);
                }
                Err(error) if error.downcast_ref::<crate::reads::Pending>().is_some() => {}
                Err(error) => {
                    if let Some(warning) =
                        reconcile_warning(&mut self.reconcile_errors, &key, &error)
                    {
                        eprintln!("{warning}");
                    }
                }
            }
        }
        let ended: Vec<_> = self
            .state
            .mappings
            .iter()
            .filter(|(key, m)| {
                m.active
                    && !self.agents.contains_key(*key)
                    && !m
                        .agent_record
                        .as_ref()
                        .is_some_and(|a| uncertain_terminals.contains(&a.terminal_id))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in ended {
            self.reads.set_ended(&key, true);
            let baselining = self.state.mappings[&key].forwarding_baseline_pending;
            if let Some(group) = self.state.mappings[&key].group_id_hex.clone() {
                if let Some(agent) = self.state.mappings[&key].agent_record.clone()
                    && self.collect_finals(&key, &agent, &group).await.is_err()
                {
                    eprintln!("Final reply for an ended session remains pending.");
                    continue;
                }
                self.reads.finish(&key);
                if self.config.edit_responses && self.state.mappings[&key].response_id.is_some() {
                    self.state.update_response(
                        &key,
                        &group,
                        "> ended No completed reply was recorded.",
                        true,
                    );
                }
                if !baselining {
                    self.state.enqueue(&group, "> ended This group retains its history. A restored native session will reconnect here.");
                }
            }
            let m = self.state.mappings.get_mut(&key).unwrap();
            m.active = false;
            m.pending_prompt = false;
            m.completion_pending = false;
        }
        self.save()
    }

    async fn seed_native_history(&mut self, key: &str, agent: &Agent) -> Result<bool> {
        if !self.state.mappings[key].output_initialised
            && let Some(_) = agent
                .agent_session
                .as_ref()
                .filter(|session| matches!(session.agent.as_str(), "codex" | "claude"))
        {
            let finals = self.read_history(agent).await?.finals;
            let mapping = self.state.mappings.get_mut(key).unwrap();
            mapping
                .final_ids
                .extend(finals.into_iter().map(|reply| reply.id));
            mapping.output_initialised = true;
            mapping.last_export_at = now();
            self.save()?;
            return Ok(true);
        }
        Ok(false)
    }

    async fn collect_finals(&mut self, key: &str, agent: &Agent, group: &str) -> Result<()> {
        if self.baseline_forwarding(key, agent).await? {
            return Ok(());
        }
        // The ended-session path also reaches this method without live reconciliation.
        if self.seed_native_history(key, agent).await? {
            return Ok(());
        }
        if agent
            .agent_session
            .as_ref()
            .is_some_and(|s| output::supported(&s.agent))
        {
            let history = self.read_history(agent).await?;
            if !self.state.forwarding_enabled {
                let mapping = self.state.mappings.get_mut(key).unwrap();
                let muted: HashSet<_> = mapping.muted_turns.iter().cloned().collect();
                mapping.muted_turns.extend(
                    history
                        .turns
                        .into_iter()
                        .filter(|turn| !muted.contains(turn)),
                );
            }
            let known: HashSet<_> = self.state.mappings[key].final_ids.iter().cloned().collect();
            for reply in history.finals {
                if !known.contains(&reply.id) {
                    if reply
                        .turn
                        .as_ref()
                        .is_none_or(|turn| !self.state.mappings[key].muted_turns.contains(turn))
                    {
                        if self.config.edit_responses {
                            self.state.complete_progress(key);
                        }
                        self.state.enqueue(group, &reply.text);
                    }
                    self.state
                        .mappings
                        .get_mut(key)
                        .unwrap()
                        .final_ids
                        .push(reply.id);
                }
            }
            self.state.mappings.get_mut(key).unwrap().last_export_at = now();
        }
        Ok(())
    }

    async fn reconcile_agent(&mut self, key: &str, agent: &Agent) -> Result<()> {
        if self.reads.background
            && agent
                .agent_session
                .as_ref()
                .is_some_and(|s| output::supported(&s.agent))
        {
            let needs_read = self.state.mappings.get(key).is_none_or(|mapping| {
                !mapping.output_initialised
                    || mapping.forwarding_baseline_pending
                    || (self.config.edit_responses && agent.agent_status == "working")
                    || (agent.agent_status == "blocked"
                        && agent
                            .agent_session
                            .as_ref()
                            .is_some_and(|s| s.agent == "opencode")
                        && (mapping.blocked_context_seq != Some(agent.state_change_seq)
                            || now().saturating_sub(mapping.decision_checked_at) >= 5))
                    || (agent.settled()
                        && (mapping.last_state_change_seq != agent.state_change_seq
                            || mapping.last_agent_status != agent.agent_status
                            || mapping.completion_pending
                            || now().saturating_sub(mapping.last_export_at) >= 30))
            });
            if needs_read {
                let result = self
                    .reads
                    .read(&self.config, agent, self.state.forwarding_revision.clone())
                    .await;
                if let Err(error) = result {
                    // Pending reads are silent. Progress/blocked-state updates remain
                    // available when optional task or decision extraction fails.
                    if !matches!(agent.agent_status.as_str(), "blocked" | "working")
                        || error.downcast_ref::<crate::reads::Pending>().is_some()
                    {
                        return Err(error);
                    }
                }
            }
        }
        if !self.state.mappings.contains_key(key) {
            // Capture the initial final-message watermark before exposing a new group.
            let mut m = Mapping {
                joined_at: now(),
                active: true,
                agent_record: Some(agent.clone()),
                ..Mapping::default()
            };
            if agent
                .agent_session
                .as_ref()
                .is_some_and(|s| output::supported(&s.agent))
            {
                let finals = self.read_history(agent).await?.finals;
                m.final_ids = finals.into_iter().map(|f| f.id).collect();
                m.output_initialised = true;
                if self.config.edit_responses && agent.settled() {
                    m.task_revision = self
                        .read_tasks(agent)
                        .await
                        .ok()
                        .flatten()
                        .map(|tasks| tasks.revision);
                }
            }
            self.state.mappings.insert(key.to_owned(), m);
            self.save()?;
        }
        self.baseline_forwarding(key, agent).await?;
        // Older bridge versions created these groups without a transcript watermark.
        // Seed their existing history once when enabling native reply support.
        self.seed_native_history(key, agent).await?;
        self.refresh_forwarding()?;
        let m = &self.state.mappings[key];
        if m.group_id_hex.is_none() && !m.creating && self.state.forwarding_enabled {
            self.state.mappings.get_mut(key).unwrap().creating = true;
            self.save()?;
            // No blind retries: GroupCreate has no idempotency key, even after a timeout.
            match self.marmot.marmot(json!({"type":"group_create", "account_id_hex":self.config.account_id_hex,"name":agent.group_name(),"members":[self.config.phone_npub],"description":agent.group_description()})).await {
                    Ok(response) => {
                        ensure!(response["type"] == "group_created", "Unexpected group creation response");
                        let group = response["group_id_hex"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("Missing group ID"))?.to_owned();
                        let m = self.state.mappings.get_mut(key).unwrap(); m.group_id_hex = Some(group.clone()); m.creating = false;
                        self.state.enqueue(&group, &format!("Connected to HERDR: {}. Send a message here to prompt this session. Use /status for its state.", agent.title()));
                        self.save()?;
                    }
                    Err(error) => {
                        if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() {
                            self.state.mappings.get_mut(key).unwrap().creating = false; self.save()?;
                        } else { eprintln!("Group creation needs inspection. Use status and resolve-create before retrying."); }
                        return Ok(());
                    }
                }
        }
        let Some(group) = self.state.mappings[key].group_id_hex.clone() else {
            return Ok(());
        };
        let previous = &self.state.mappings[key];
        let changed = previous.last_agent_status != agent.agent_status
            || previous.last_state_change_seq != agent.state_change_seq;
        let status_changed = previous.last_agent_status != agent.agent_status;
        let resumed = previous.last_agent_status == "blocked";
        let previous_settled = matches!(previous.last_agent_status.as_str(), "idle" | "done");
        if self.config.edit_responses {
            if agent.agent_status == "working" {
                if previous_settled
                    && !self.state.mappings[key].pending_prompt
                    && self.state.mappings[key].response_id.is_some()
                {
                    self.state.update_response(
                        key,
                        &group,
                        "> idle No completed reply was recorded.",
                        true,
                    );
                }
                self.state.begin_response(key, &group);
                if resumed {
                    self.state.update_response(key, &group, "> working", false);
                }
            } else if status_changed
                && !agent.settled()
                && self.state.mappings[key].response_id.is_some()
            {
                self.state.update_response(
                    key,
                    &group,
                    &format!("> {}", agent.agent_status),
                    false,
                );
            }
        } else if previous.last_agent_status != agent.agent_status && agent.agent_status != "done" {
            self.state
                .enqueue(&group, &format!("> {}", agent.agent_status));
        }
        let m = self.state.mappings.get_mut(key).unwrap();
        m.last_agent_status = agent.agent_status.clone();
        m.last_state_change_seq = agent.state_change_seq;
        m.active = true;
        m.agent_record = Some(agent.clone());
        if changed && agent.settled() {
            m.completion_pending = true;
        }
        self.save()?;
        if self.config.edit_responses
            && self.state.forwarding_enabled
            && agent.agent_status == "working"
            && agent.agent_session.is_some()
        {
            match self.read_tasks(agent).await {
                Ok(Some(tasks))
                    if self.state.mappings[key].task_revision.as_ref() != Some(&tasks.revision)
                        || resumed =>
                {
                    self.state
                        .update_response(key, &group, &tasks.text(), false);
                    self.state.mappings.get_mut(key).unwrap().task_revision = Some(tasks.revision);
                }
                Ok(_) => {}
                Err(_) => eprintln!("Task list unavailable; response delivery remains active."),
            }
        }
        let opencode = agent
            .agent_session
            .as_ref()
            .is_some_and(|session| session.agent == "opencode");
        if self.state.forwarding_enabled && agent.agent_status == "blocked" && opencode {
            let mapping = &self.state.mappings[key];
            if mapping.blocked_context_seq != Some(agent.state_change_seq)
                || (mapping
                    .decision
                    .as_ref()
                    .is_none_or(|decision| !decision.announced)
                    || mapping.decision_refresh_pending)
                    && now().saturating_sub(mapping.decision_checked_at) >= 5
            {
                let decision = self
                    .reads
                    .read(&self.config, agent, self.state.forwarding_revision.clone())
                    .await
                    .map(|snapshot| snapshot.decision.clone());
                self.state
                    .mappings
                    .get_mut(key)
                    .unwrap()
                    .decision_checked_at = now();
                self.state
                    .mappings
                    .get_mut(key)
                    .unwrap()
                    .decision_refresh_pending = !matches!(&decision, Ok(Some(_)));
                match decision {
                    Ok(Some(decision)) => {
                        self.announce_decision(key, &group, decision)?;
                    }
                    _ if self.state.mappings[key].blocked_context_seq
                        != Some(agent.state_change_seq) =>
                    {
                        let notice = "> blocked Agent needs input in the terminal. No supported decision is available yet.";
                        if self.config.edit_responses {
                            self.state.update_response(key, &group, notice, false);
                        } else {
                            self.state.enqueue(&group, notice);
                        }
                    }
                    _ => {}
                }
                self.state
                    .mappings
                    .get_mut(key)
                    .unwrap()
                    .blocked_context_seq = Some(agent.state_change_seq);
            }
        } else if agent.agent_status == "blocked"
            && self.state.mappings[key].blocked_context_seq != Some(agent.state_change_seq)
        {
            let response = self
                .herdr
                .herdr(
                    "agent.read",
                    json!({"target":agent.pane_id,"source":"visible","format":"text","lines":40}),
                )
                .await?;
            herdr::get(&self.herdr, agent).await?;
            if let Some(text) = response["result"]["read"]["text"].as_str() {
                let text = format!("> blocked Agent needs input (terminal context):\n{text}");
                if self.config.edit_responses {
                    self.state.update_response(key, &group, &text, false);
                } else {
                    self.state.enqueue(&group, &text);
                }
            }
            self.state
                .mappings
                .get_mut(key)
                .unwrap()
                .blocked_context_seq = Some(agent.state_change_seq);
        }
        if agent.settled()
            && (self.state.mappings[key].completion_pending
                || now().saturating_sub(self.state.mappings[key].last_export_at) >= 30)
        {
            self.collect_finals(key, agent, &group).await?;
            if self.config.edit_responses {
                if self.state.mappings[key].response_id.is_some() {
                    self.state.update_response(
                        key,
                        &group,
                        "> idle Waiting for the completed reply.",
                        false,
                    );
                }
                if agent.agent_session.is_some() {
                    self.state.mappings.get_mut(key).unwrap().task_revision = self
                        .read_tasks(agent)
                        .await
                        .ok()
                        .flatten()
                        .map(|tasks| tasks.revision);
                }
            }
            if self.state.mappings[key].completion_pending {
                let awaiting_final = self.config.edit_responses
                    && self.state.mappings[key].response_id.is_some()
                    && agent
                        .agent_session
                        .as_ref()
                        .is_some_and(|session| output::supported(&session.agent));
                self.state.mappings.get_mut(key).unwrap().pending_prompt = awaiting_final;
                self.state.mappings.get_mut(key).unwrap().completion_pending = awaiting_final;
            }
        }
        let m = self.state.mappings.get_mut(key).unwrap();
        m.last_agent_status = agent.agent_status.clone();
        m.last_state_change_seq = agent.state_change_seq;
        m.active = true;
        m.agent_record = Some(agent.clone());
        self.save()?;
        Ok(())
    }

    async fn read_history(&mut self, agent: &Agent) -> Result<output::SessionHistory> {
        Ok(self
            .reads
            .read(&self.config, agent, self.state.forwarding_revision.clone())
            .await?
            .history
            .clone())
    }

    async fn read_tasks(&mut self, agent: &Agent) -> Result<Option<output::tasks::TaskList>> {
        Ok(self
            .reads
            .read(&self.config, agent, self.state.forwarding_revision.clone())
            .await?
            .tasks
            .clone())
    }

    /// Schedule independent transcript reads without waiting for their completion.
    pub async fn poll_sessions(&mut self, agents: Vec<Agent>) -> Result<()> {
        self.reads.background = true;
        self.reconcile(agents).await
    }

    pub async fn poll_ready_sessions(&mut self) -> Result<()> {
        // Completion wakes apply existing reads without starting another polling round.
        // Preserve unknown-session terminals from the authoritative HERDR snapshot.
        self.reads.scheduling = false;
        let result = self.reconcile(self.last_snapshot.clone()).await;
        self.reads.scheduling = true;
        result
    }

    pub async fn inbound(&mut self, event: &Value) -> Result<()> {
        self.route_inbound(event, false).await
    }

    async fn route_inbound(&mut self, event: &Value, prepared: bool) -> Result<()> {
        ensure!(
            event["marmot_agent_control"] == PROTOCOL,
            "MDK event protocol mismatch"
        );
        if event["type"] != "inbound_message"
            || event["account_id_hex"] != self.config.account_id_hex
        {
            return Ok(());
        }
        let msg = &event["message"];
        if msg["sender"]["account_id_hex"] != self.phone || msg["sender"]["is_self"] == true {
            return Ok(());
        }
        let Some(group) = event["group_id_hex"].as_str() else {
            return Ok(());
        };
        let Some(key) = self
            .state
            .mappings
            .iter()
            .find(|(_, m)| m.group_id_hex.as_deref() == Some(group))
            .map(|(k, _)| k.clone())
        else {
            return Ok(());
        };
        let Some(id) = msg["message_id_hex"].as_str() else {
            return Ok(());
        };
        if self.state.mappings[&key]
            .seen_messages
            .iter()
            .any(|seen| seen == id)
        {
            return Ok(());
        }
        if let Some(pending) = self.state.media_pending.iter_mut().find(|pending| {
            !prepared
                && pending["group_id_hex"] == group
                && pending["message"]["message_id_hex"] == id
        }) {
            if !crate::media::complete_refs(&pending["message"]) && crate::media::complete_refs(msg)
            {
                pending["message"] = msg.clone();
                pending["_codr_retry_at"] = json!(0);
                self.save()?;
            }
            return Ok(());
        }
        self.refresh_forwarding()?;
        forwarding::wake(&self.config.state_path, group, id)?;
        self.refresh_forwarding()?;
        ensure!(
            self.herdr_available,
            "HERDR unavailable; inbound message deferred until recovery"
        );
        self.activate_forwarding().await?;
        ensure!(
            !self.state.forwarding_resume_pending,
            "Forwarding resume awaits its current history checkpoint"
        );
        ensure!(
            self.agents.contains_key(&key) || !self.state.mappings[&key].active,
            "Session identity is uncertain; phone input deferred"
        );
        if let Some(agent) = self.agents.get(&key).cloned() {
            self.baseline_forwarding(&key, &agent).await?;
        }
        let text = msg["text"].as_str().unwrap_or("");
        if text != "/status"
            && !text.trim().is_empty()
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["media"].as_array().is_none_or(|media| media.is_empty())
            && let Some(agent) = self.agents.get(&key).cloned()
            && self.answer_decision(&key, &agent, group, id, text).await?
        {
            return Ok(());
        }
        // A final transcript record can lag HERDR's idle state. Resolve it before
        // opening another response, so it cannot overwrite the next prompt's message.
        if self.config.edit_responses
            && text != "/status"
            && self.state.mappings[&key].pending_prompt
            && self.state.mappings[&key].response_id.is_some()
            && let Some(agent) = self.agents.get(&key).cloned().filter(Agent::settled)
        {
            self.collect_finals(&key, &agent, group).await?;
            if self.state.mappings[&key].response_id.is_none() {
                let mapping = self.state.mappings.get_mut(&key).unwrap();
                mapping.pending_prompt = false;
                mapping.completion_pending = false;
            }
            self.save()?;
        }
        if text != "/status"
            && !text.trim().is_empty()
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["media"].as_array().is_none_or(|a| a.is_empty())
            && self.agents.get(&key).is_some_and(Agent::settled)
            && !self.state.mappings[&key].pending_prompt
        {
            let agent = self.agents[&key].clone();
            match herdr::get(&self.herdr, &agent).await {
                Ok(current) if current.settled() => {}
                Err(error) if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() => {
                    self.herdr_available = false;
                    return Err(error);
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "Agent readiness changed; recover message after a fresh snapshot"
                    ));
                }
            }
        }
        let media_busy = self.state.media_pending.iter().any(|pending| {
            pending["group_id_hex"] == group
                && !(prepared && pending["message"]["message_id_hex"] == id)
        });
        if text != "/status"
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["media"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            && !media_busy
            && self.state.media_pending.len() < 16
            && self.agents.get(&key).is_some_and(|agent| {
                (agent.settled() && !self.state.mappings[&key].pending_prompt)
                    || (agent.agent_status == "blocked" && crate::media::audio(msg))
            })
        {
            let mut pending = event.clone();
            if crate::media::audio(msg) && self.config.edit_responses {
                pending["_codr_transcription_notice"] =
                    json!(self.state.begin_transcription(group));
            } else {
                self.state.enqueue(
                    group,
                    if crate::media::audio(msg) {
                        "> transcribing"
                    } else {
                        "> downloading"
                    },
                );
            }
            self.state.media_pending.push(pending);
            return self.save();
        }
        // Persist consumption before terminal input: uncertain prompts must not run twice.
        let m = self.state.mappings.get_mut(&key).unwrap();
        m.seen_messages.push(id.into());
        self.save()?;
        let response = if text == "/status" {
            Some(format!(
                "> {} HERDR session: {}.",
                self.state.mappings[&key].last_agent_status,
                if self.agents.contains_key(&key) {
                    "connected"
                } else {
                    "ended"
                }
            ))
        } else if msg["text_truncated"] == true || msg["attachments_truncated"] == true {
            Some("This offline message was truncated by MDK. No prompt was sent. Send a shorter text message.".into())
        } else if !msg["media"].as_array().is_none_or(|a| a.is_empty()) {
            Some("Media could not be queued. The session may be busy or ended. Wait for it to become idle and try again.".into())
        } else if let Some(agent) = self.agents.get(&key).cloned() {
            if media_busy || self.state.mappings[&key].pending_prompt || !agent.settled() {
                Some("The agent is busy or blocked. Wait for it to become idle before sending another prompt.".into())
            } else if text.trim().is_empty() {
                Some("Send a non-empty text message.".into())
            } else {
                self.state.mappings.get_mut(&key).unwrap().pending_prompt = true;
                self.save()?;
                match herdr::prompt(&self.herdr, &agent, text).await {
                    Ok(_) => {
                        if self.config.edit_responses {
                            if self.state.mappings[&key].response_id.is_some() {
                                self.state.update_response(
                                    &key,
                                    group,
                                    "> idle No completed reply was recorded.",
                                    true,
                                );
                            }
                            self.state.begin_response(&key, group);
                        }
                        None
                    }
                    Err(error) => {
                        if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() {
                            let m = self.state.mappings.get_mut(&key).unwrap();
                            m.pending_prompt = false;
                            m.seen_messages.retain(|seen| seen != id);
                            self.herdr_available = false;
                            self.save()?;
                            return Err(error);
                        }
                        Some("The prompt could not be confirmed. It will not be retried. Inspect the agent before submitting again. The local clear-prompt command can release an uncertain prompt.".into())
                    }
                }
            }
        } else {
            Some("This session has ended. No prompt was sent.".into())
        };
        if let Some(text) = response {
            self.state.enqueue(group, &text);
        }
        self.save()
    }

    /// Process one download/transcription at a time without blocking other sessions.
    pub async fn process_media(&mut self) -> Result<()> {
        let mut routing_result = Ok(());
        if self.media_job.as_ref().is_some_and(|job| job.is_finished()) {
            let result = self.media_job.take().unwrap().await;
            if matches!(&result, Ok(Err(error)) if error.downcast_ref::<crate::media::MediaUnavailable>().is_some())
            {
                // A live event may have hydrated a summary-only history row while
                // lookup was in flight. Never consume an incomplete recovery row.
                if !crate::media::complete_refs(&self.state.media_pending[0]["message"]) {
                    self.state.media_pending[0]["_codr_retry_at"] = json!(now() + 60);
                    self.save()?;
                    return Ok(());
                }
            } else {
                match result {
                    Ok(Ok(prepared)) => {
                        // Retain references and prepared text until inbound consumption.
                        self.state.media_pending[0]["_codr_prepared_text"] = json!(prepared.prompt);
                        if let Some(transcript) = prepared.transcript {
                            let event = self.state.media_pending[0].clone();
                            self.finish_transcription_notice(
                                &event,
                                &crate::media::transcript_notice(&transcript),
                            );
                        }
                        self.save()?;
                    }
                    _ => {
                        let event = self.state.media_pending.remove(0);
                        self.finish_transcription_notice(&event, "> transcription failed");
                        let group = event["group_id_hex"].as_str().unwrap_or("");
                        if let Some(mapping) = self
                            .state
                            .mappings
                            .values_mut()
                            .find(|m| m.group_id_hex.as_deref() == Some(group))
                        {
                            if let Some(id) = event["message"]["message_id_hex"].as_str() {
                                mapping.seen_messages.push(id.into());
                            }
                            self.state.enqueue(group, "Media processing failed. No prompt was sent. Check the local media/transcription service and send the message again.");
                        }
                        self.save()?;
                    }
                }
            }
        }
        if self.media_job.is_none()
            && let Some(index) = self.state.media_pending.iter().position(|event| {
                event["_codr_prepared_text"].is_string()
                    && event["_codr_retry_at"].as_u64().unwrap_or(0) <= now()
            })
        {
            let mut event = self.state.media_pending[index].clone();
            event["message"]["text"] = event["_codr_prepared_text"].clone();
            event["message"]["media"] = json!([]);
            let result = self.route_inbound(&event, true).await;
            if self.state.mappings.values().any(|mapping| {
                mapping.group_id_hex.as_deref() == event["group_id_hex"].as_str()
                    && mapping
                        .seen_messages
                        .iter()
                        .any(|id| Some(id.as_str()) == event["message"]["message_id_hex"].as_str())
            }) {
                self.state.media_pending.remove(index);
                self.save()?;
            } else if result.is_err() {
                // Defer this session without starving prepared input for other chats.
                let mut pending = self.state.media_pending.remove(index);
                pending["_codr_retry_at"] = json!(now() + 1);
                self.state.media_pending.push(pending);
                self.save()?;
            }
            routing_result = result;
        }
        if self.media_job.is_none()
            && let Some(index) = self.state.media_pending.iter().position(|event| {
                !event["_codr_prepared_text"].is_string()
                    && event["_codr_retry_at"].as_u64().unwrap_or(0) <= now()
            })
        {
            // A deferred message must not block preparation for other chats.
            self.state.media_pending.rotate_left(index);
            let event = &self.state.media_pending[0];
            let config = self.config.clone();
            let socket = self.marmot.clone();
            let event = event.clone();
            self.media_job = Some(tokio::spawn(async move {
                crate::media::prepare(
                    &config,
                    &socket,
                    event["group_id_hex"].as_str().unwrap_or(""),
                    &event["message"],
                )
                .await
            }));
        }
        routing_result
    }

    fn finish_transcription_notice(&mut self, event: &Value, text: &str) {
        if !crate::media::audio(&event["message"]) {
            return;
        }
        if let Some(id) = event["_codr_transcription_notice"].as_str() {
            if let Some(item) = self.state.outbox.iter_mut().find(|item| item.id == id)
                && let Some(delivery) = item.response.as_mut()
            {
                item.text = text.into();
                delivery.finished = true;
            }
        } else {
            self.state
                .enqueue(event["group_id_hex"].as_str().unwrap_or(""), text);
        }
    }

    fn announce_decision(
        &mut self,
        key: &str,
        group: &str,
        mut decision: output::decision::Decision,
    ) -> Result<()> {
        if let Some(previous) = self.state.mappings[key].decision.as_ref() {
            if previous.same_request(&decision) {
                decision = previous.clone();
            } else {
                decision.requires_token = !previous.submitted;
            }
        }
        if !decision.announced && self.state.forwarding_enabled {
            self.state.enqueue(group, &decision.text());
            decision.announced = true;
            if self.config.edit_responses {
                self.state.update_response(
                    key,
                    group,
                    "> blocked Waiting for your decision.",
                    false,
                );
            }
        }
        self.state.mappings.get_mut(key).unwrap().decision = Some(decision);
        self.save()
    }

    fn decision_notice(&mut self, key: &str, group: &str, id: &str, text: &str) -> Result<bool> {
        self.state
            .mappings
            .get_mut(key)
            .unwrap()
            .seen_messages
            .push(id.into());
        self.state.enqueue(group, text);
        self.save()?;
        Ok(true)
    }

    async fn answer_decision(
        &mut self,
        key: &str,
        agent: &Agent,
        group: &str,
        id: &str,
        text: &str,
    ) -> Result<bool> {
        let Some(session) = agent
            .agent_session
            .as_ref()
            .filter(|session| session.agent == "opencode")
        else {
            return Ok(false);
        };
        let previous = self.state.mappings[key].decision.clone();
        let numeric = text.trim().bytes().all(|byte| byte.is_ascii_digit());
        let looks_like_answer = numeric
            || text.trim().starts_with("/answer ")
            || previous
                .as_ref()
                .is_some_and(|decision| decision.looks_like_answer(text));
        if agent.agent_status != "blocked" && previous.is_none() && !looks_like_answer {
            return Ok(false);
        }
        let current = herdr::get(&self.herdr, agent).await?;
        if current.agent_status != "blocked" {
            if previous.is_some() && looks_like_answer {
                return self.decision_notice(
                    key,
                    group,
                    id,
                    "That decision is no longer waiting. No answer was sent.",
                );
            }
            return Ok(false);
        }
        let active = if self.config.opencode_db.is_some() {
            self.reads.fresh(&self.config, session).await?.decision
        } else {
            output::decision::read(&self.config, session).await?
        };
        let Some(active) = active else {
            if looks_like_answer || previous.is_some() {
                return self.decision_notice(key, group, id, "No supported decision is available yet. Wait for its question card or use the terminal. No answer was sent.");
            }
            return Ok(false);
        };
        if previous
            .as_ref()
            .is_none_or(|previous| !previous.announced || !previous.same_request(&active))
        {
            self.announce_decision(key, group, active)?;
            return self.decision_notice(key, group, id, "The current decision is shown above. Please reply to that question; no answer was sent yet.");
        }
        let decision = previous.unwrap();
        if decision.submitted {
            return self.decision_notice(key, group, id, "An answer was already submitted for this question. Check the terminal if it is still waiting.");
        }
        let choice = match decision.choice(text) {
            Ok(choice) => choice,
            Err(error) => return self.decision_notice(key, group, id, &error.to_string()),
        };
        let visible = self.herdr.herdr("agent.read", json!({"target":agent.pane_id,"source":"visible","format":"text","lines":300,"strip_ansi":true})).await?;
        let matches = visible["result"]["read"]["truncated"] != true
            && visible["result"]["read"]["text"]
                .as_str()
                .is_some_and(|text| decision.matches_visible(text));
        if !matches || herdr::get(&self.herdr, agent).await?.agent_status != "blocked" {
            return self.decision_notice(key, group, id, "The terminal is no longer showing that decision's choice list. No answer was sent. Check the terminal or wait for its current question.");
        }
        let pending_prompt = self.state.mappings[key].pending_prompt;
        let mapping = self.state.mappings.get_mut(key).unwrap();
        mapping.decision.as_mut().unwrap().submitted = true;
        mapping.pending_prompt = true;
        mapping.seen_messages.push(id.into());
        self.save()?;
        match herdr::choose(&self.herdr, agent, choice).await {
            Ok(()) => {
                self.state.enqueue(group, &format!("Sent choice {choice}: {}.", decision.questions[0].options[choice-1].label));
            }
            Err(error) if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() => {
                let mapping = self.state.mappings.get_mut(key).unwrap();
                mapping.decision.as_mut().unwrap().submitted = false;
                mapping.pending_prompt = pending_prompt;
                mapping.seen_messages.retain(|seen| seen != id);
                self.herdr_available = false;
                self.save()?;
                return Err(error);
            }
            Err(_) => self.state.enqueue(group, "The decision answer could not be confirmed. It will not be retried automatically. Inspect the terminal before using clear-prompt and sending a new answer."),
        }
        self.save()?;
        Ok(true)
    }

    pub async fn recover_inbound(&mut self) -> Result<()> {
        let groups: Vec<_> = self
            .state
            .mappings
            .iter()
            .filter_map(|(key, m)| m.group_id_hex.as_ref().map(|g| (key.clone(), g.clone())))
            .collect();
        let mut failed = false;
        for (_, group) in groups {
            if self.recover_group(&group).await.is_err() {
                failed = true;
            }
        }
        ensure!(!failed, "Some groups still need inbound recovery");
        Ok(())
    }

    async fn recover_group(&mut self, group: &str) -> Result<()> {
        // Sender timestamps are not ingestion cursors. Rescan retained history and deduplicate
        // by durable message ID so delayed relay deliveries cannot fall behind a watermark.
        let mut before: Option<Value> = None;
        let mut pages = Vec::new();
        loop {
            let mut req = json!({"type":"timeline_list","account_id_hex":self.config.account_id_hex,"group_id_hex":group,"limit":50});
            if let Some(cursor) = &before {
                req["before"] = cursor.clone();
            }
            let page = self.marmot.marmot(req).await?;
            ensure!(page["type"] == "timeline_page", "Invalid timeline response");
            let rows = page["messages"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing timeline messages"))?
                .clone();
            if rows.is_empty() {
                break;
            }
            let cursor = json!({"recorded_at":rows[0]["recorded_at"], "message_id_hex":rows[0]["message_id_hex"]});
            ensure!(
                before.as_ref() != Some(&cursor),
                "Timeline cursor did not advance"
            );
            before = Some(cursor);
            pages.push(rows);
            if page["has_more_before"] != true {
                break;
            }
        }
        for rows in pages.into_iter().rev() {
            for row in rows {
                if row["availability"] == "available" && row["kind"] == 9 {
                    self.inbound(&json!({"marmot_agent_control":PROTOCOL,"type":"inbound_message","account_id_hex":self.config.account_id_hex,"group_id_hex":group,"message":{"message_id_hex":row["message_id_hex"],"sender":row["sender"],"text":row["text"],"text_truncated":row["text_truncated"],"attachments_truncated":row["attachments_truncated"],"recorded_at":row["recorded_at"],"media":row["attachments"]}})).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn rename_groups(&mut self) -> Result<()> {
        let live: BTreeMap<_, _> = herdr::snapshot(&self.herdr)
            .await?
            .into_iter()
            .map(|a| (a.key(), a))
            .collect();
        let keys: Vec<_> = self.state.mappings.keys().cloned().collect();
        for key in keys {
            let mapping = &self.state.mappings[&key];
            let Some(group) = mapping.group_id_hex.clone() else {
                continue;
            };
            let agent = live.get(&key).or(mapping.agent_record.as_ref()).cloned();
            let info = self.marmot.marmot(json!({"type":"group_info","account_id_hex":self.config.account_id_hex,"group_id_hex":group})).await?;
            ensure!(
                info["type"] == "group_info",
                "Unexpected MDK group response"
            );
            let old = info["subject"].as_str().unwrap_or("session");
            let name = if let Some(agent) = agent
                .as_ref()
                .filter(|a| a.tab_label.is_some() || live.contains_key(&key))
            {
                agent.group_name()
            } else {
                let label = old
                    .strip_prefix("[cd] ")
                    .or_else(|| old.strip_prefix("[codr] "))
                    .or_else(|| old.strip_prefix("HDR | "))
                    .unwrap_or(old);
                herdr::group_name(label.strip_suffix(" · codr").unwrap_or(label))
            };
            let description = agent
                .as_ref()
                .map(Agent::group_description)
                .unwrap_or_else(|| {
                    "Workspace: unknown\nAgent: unknown\nManaged by codr via HERDR.".into()
                });
            // MDK 0.13 exposes the subject but not the description. Remember only
            // confirmed metadata writes, so matching titles can still gain details.
            if info["subject"] == name
                && info["description"]
                    .as_str()
                    .or(mapping.group_description.as_deref())
                    == Some(description.as_str())
            {
                continue;
            }
            // Profile updates have no idempotency key. Never retry an uncertain mutation.
            let result = self.marmot.marmot(json!({"type":"group_profile_update","account_id_hex":self.config.account_id_hex,"group_id_hex":group,"name":name,"description":description})).await?;
            ensure!(
                result["type"] == "group_profile_updated"
                    && result["message_ids_hex"]
                        .as_array()
                        .is_some_and(|ids| !ids.is_empty()),
                "MDK did not confirm the group rename"
            );
            let info = self.marmot.marmot(json!({"type":"group_info","account_id_hex":self.config.account_id_hex,"group_id_hex":group})).await?;
            ensure!(
                info["type"] == "group_info"
                    && info["subject"] == name
                    && info["description"]
                        .as_str()
                        .is_none_or(|actual| actual == description),
                "Renamed group title could not be verified"
            );
            self.state.mappings.get_mut(&key).unwrap().group_description = Some(description);
            self.save()?;
            println!("Renamed: {name}");
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        self.reads.background = true;
        let reads_ready = self.reads.notify.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(256);
        let marmot = self.marmot.clone();
        let account = self.config.account_id_hex.clone();
        let subscriber = tokio::spawn(async move {
            loop {
                if let Ok(mut reader) = marmot.subscribe(json!({"type":"subscribe_inbound","account_id_hex":account,"group_id_hex":null}), true).await {
                    if tx.send(json!({"type":"reconcile_inbound"})).await.is_err() { return; }
                    while let Ok(event) = read_line(&mut reader).await {
                        if tx.send(event).await.is_err() { return; }
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        // Authoritative snapshots avoid relying on HERDR's non-durable, differently shaped event payloads.
        // State-change sequence and stored assistant message IDs also catch work completed between snapshots.
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut recovery_due = true;
        let mut recovery_tick = tokio::time::interval(Duration::from_secs(60));
        recovery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    subscriber.abort();
                    if let Some(job) = self.media_job.take() { job.abort(); }
                    return Ok(());
                },
                _ = tick.tick() => {
                    match herdr::snapshot(&self.herdr).await {
                        Ok(agents) => if self.reconcile(agents).await.is_err() { eprintln!("Session reconciliation failed; will retry."); },
                        Err(_) => { self.herdr_available = false; recovery_due = true; eprintln!("HERDR is unavailable; waiting for reconnection."); }
                    }
                },
                _ = reads_ready.notified() => {
                    if self.herdr_available && self.poll_ready_sessions().await.is_err() {
                        eprintln!("Session reads remain pending.");
                    }
                },
                Some(event) = rx.recv() => {
                    if event["type"] == "reconcile_inbound" || event["type"] == "resync_required" {
                        // Refresh current agent identities before recovering any queued phone prompts.
                        recovery_due = true;
                    } else if self.inbound(&event).await.is_err() { recovery_due = true; eprintln!("Inbound message deferred; will reconcile history."); }
                },
                _ = recovery_tick.tick() => { recovery_due = true; }
            }
            if recovery_due && self.herdr_available {
                recovery_due = self.recover_inbound().await.is_err();
            }
            if self.process_media().await.is_err() {
                recovery_due = true;
                eprintln!("Prepared media input deferred; will reconcile history.");
            }
            if self.flush().await.is_err() {
                eprintln!("MDK send unavailable; private outbox retained for retry.");
            }
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::reconcile_warning;
    use anyhow::anyhow;
    use std::collections::BTreeMap;

    #[test]
    fn repeated_identical_failures_are_reported_once() {
        let mut errors = BTreeMap::new();
        assert_eq!(
            reconcile_warning(&mut errors, "session-a", &anyhow!("boom")).as_deref(),
            Some("Session session-a could not reconcile: boom")
        );
        assert!(reconcile_warning(&mut errors, "session-a", &anyhow!("boom")).is_none());
        assert_eq!(
            reconcile_warning(&mut errors, "session-a", &anyhow!("different failure")).as_deref(),
            Some("Session session-a could not reconcile: different failure")
        );
        assert_eq!(
            reconcile_warning(&mut errors, "session-b", &anyhow!("boom")).as_deref(),
            Some("Session session-b could not reconcile: boom")
        );
    }
}
