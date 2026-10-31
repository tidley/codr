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

const CONTROL_KEY: &str = "control";

pub struct Bridge {
    pub config: BridgeConfig,
    pub state: BridgeState,
    pub agents: BTreeMap<String, Agent>,
    pub herdr: Socket,
    pub marmot: Socket,
    pub herdr_available: bool,
    /// Set by the executable for managed worker updates; no installer path is inferred.
    pub update_config_path: Option<std::path::PathBuf>,
    pub update_messaging_home: Option<std::path::PathBuf>,
    pub(crate) update_check: Option<crate::update::Check>,
    phone: String,
    media_job: Option<tokio::task::JoinHandle<Result<crate::media::Prepared>>>,
    reads: crate::reads::Reads,
    pub(crate) last_snapshot: Vec<Agent>,
    pub(crate) porter_updates: tokio::sync::watch::Sender<Value>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(check) = &self.update_check {
            check.task.abort();
        }
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
            update_config_path: None,
            update_messaging_home: None,
            update_check: None,
            phone,
            media_job: None,
            reads: crate::reads::Reads::default(),
            last_snapshot: vec![],
            porter_updates: tokio::sync::watch::channel(Value::Null).0,
        };
        bridge.refresh_forwarding()?;
        if bridge.config.control_chat {
            let mapping = bridge.state.mappings.entry(CONTROL_KEY.into()).or_default();
            let changed = !mapping.control || !mapping.active;
            mapping.control = true;
            mapping.active = true;
            if changed {
                bridge.save()?;
            }
        }
        Ok(bridge)
    }

    pub fn save(&self) -> Result<()> {
        self.state.save(&self.config.state_path)?;
        crate::porter::publish(self);
        Ok(())
    }

    fn refresh_forwarding(&mut self) -> Result<()> {
        let control = forwarding::load(&self.config.state_path)?;
        if self.state.forwarding_revision.as_deref() != Some(&control.revision) {
            let before = self.state.clone();
            // Preserve the old outbox on the first upgrade, unless a mode was already selected.
            let reset = self.state.forwarding_revision.is_some()
                || control.revision != "initial"
                || !control.enabled;
            let mut cancelled_images = Vec::new();
            if reset {
                cancelled_images.extend(
                    self.state
                        .outbox
                        .iter()
                        .filter(|item| !item.manual)
                        .flat_map(|item| {
                            item.attachments
                                .iter()
                                .map(|attachment| attachment.path.clone())
                        }),
                );
                let cancelled_blocks: HashSet<_> = self
                    .state
                    .outbox
                    .iter()
                    .filter(|item| {
                        !item.manual
                            && item.text.starts_with("> blocked Agent needs input")
                            && item.response.as_ref().is_none_or(|response| {
                                (response.durable
                                    || (response.target.is_none()
                                        && response.initial_text.is_none()))
                                    && response.sent_text.as_deref() != Some(item.text.as_str())
                            })
                    })
                    .map(|item| item.group.clone())
                    .collect();
                self.state.outbox.retain(|item| {
                    item.manual
                        || item.response.as_ref().is_some_and(|delivery| {
                            !delivery.durable
                                && (delivery.target.is_some() || delivery.initial_text.is_some())
                        })
                });
                for mapping in self.state.mappings.values_mut() {
                    if !self
                        .state
                        .outbox
                        .iter()
                        .any(|item| Some(&item.id) == mapping.response_id.as_ref())
                    {
                        mapping.response_id = None;
                        mapping.response_completion_pending = false;
                    }
                    mapping.task_revision = None;
                    // The control chat has no transcript and never needs a checkpoint.
                    mapping.forwarding_baseline_pending = !mapping.control;
                    // Keep the announced blocked context across mute/wake cycles;
                    // an unchanged block must not notify the phone again.
                    // A cancelled, undelivered notice still needs one delivery on wake.
                    if mapping
                        .group_id_hex
                        .as_ref()
                        .is_some_and(|group| cancelled_blocks.contains(group))
                    {
                        mapping.blocked_context_seq = None;
                    }
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
            if let Err(error) = self.save() {
                self.state = before;
                return Err(error);
            }
            // Delete only after cancellation is durable, and keep shared/manual references.
            for path in cancelled_images {
                if !self
                    .state
                    .outbox
                    .iter()
                    .any(|item| item.attachments.iter().any(|a| a.path == path))
                {
                    crate::outbound_images::remove_snapshot(&self.config.state_path, &path);
                }
            }
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
                !mapping.control
                    && mapping.forwarding_baseline_pending
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
        let mut completed_response = false;
        if let Some(history) = history {
            self.finish_queued_prompt(key, &history);
            let mapping = self.state.mappings.get_mut(key).unwrap();
            mapping.activity.baseline(&history.user_inputs);
            let known: HashSet<_> = mapping.final_ids.iter().cloned().collect();
            completed_response = history
                .finals
                .iter()
                .any(|reply| !known.contains(&reply.id));
            if completed_response && mapping.response_id.is_some() {
                mapping.response_completion_pending = true;
            }
            if mapping.queue_flight.is_none()
                && history
                    .finals
                    .iter()
                    .any(|reply| !known.contains(&reply.id))
            {
                mapping.pending_prompt = false;
            }
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
        if agent.settled()
            && mapping.queue_flight.is_none()
            && (mapping.prompt_queue.is_empty() || !mapping.pending_prompt)
        {
            mapping.pending_prompt = false;
            mapping.completion_pending = false;
            if completed_response {
                self.state.complete_progress(key);
            }
        }
        self.save()?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.refresh_forwarding()?;
        let mut failed = HashSet::new();
        let mut outbox = self.state.outbox.clone();
        // Durable replies and their continuations precede best-effort progress.
        outbox.sort_by_key(|item| (item.working_notice, !item.manual, item.response.is_some()));
        for item in outbox {
            self.refresh_forwarding()?;
            if item.manual
                && self.state.mappings.values().any(|mapping| {
                    mapping.group_id_hex.as_deref() == Some(item.group.as_str())
                        && now() < mapping.manual_reply_after
                })
            {
                continue;
            }
            if !item.manual && (!self.state.forwarding_enabled || self.activity_unresolved()) {
                continue;
            }
            if !self
                .state
                .outbox
                .iter()
                .any(|pending| pending.id == item.id)
            {
                continue;
            }
            if item.working_notice {
                // Older workers queued standalone refreshes. The active progress
                // message now carries status; do not post a second working bubble.
                self.state.confirm_send(&item.id);
                self.save()?;
                continue;
            }
            if failed.contains(&item.group) {
                continue;
            }
            if let Some(delivery) = &item.response {
                if delivery.finished && delivery.target.is_none() && !delivery.durable {
                    if delivery.initial_text.is_none() {
                        self.state.outbox.retain(|m| m.id != item.id);
                        self.save()?;
                    }
                    // An attempted send may already be visible, but transport
                    // deduplication is bounded. Retain it for inspection rather
                    // than replaying and possibly creating a replacement bubble.
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
                        if delivery.target.is_none() {
                            self.state.last_confirmed_send_at = Some(now());
                        }
                        self.save()?;
                    }
                    _ => {
                        if !delivery.finished || delivery.durable {
                            failed.insert(item.group);
                        }
                        // Retain completion targets for retry without holding durable
                        // answers or later progress in this chat behind a failed edit.
                    }
                }
                continue;
            }
            let request = if item.attachments.is_empty() {
                json!({"type":"send_final","account_id_hex":self.config.account_id_hex,"group_id_hex":item.group,"text":item.text,"reply_to_message_id_hex":null,"idempotency_key":item.id})
            } else {
                json!({"type":"send_media","account_id_hex":self.config.account_id_hex,"group_id_hex":item.group,"attachments":item.attachments,"caption":null,"idempotency_key":item.id})
            };
            let send = self.marmot.marmot(request);
            let result = send.await;
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
                    self.state.last_confirmed_send_at = Some(now());
                    self.state.confirm_send(&item.id);
                    self.save()?;
                    for attachment in &item.attachments {
                        if !self.state.outbox.iter().any(|pending| {
                            pending
                                .attachments
                                .iter()
                                .any(|other| other.path == attachment.path)
                        }) {
                            crate::outbound_images::remove_snapshot(
                                &self.config.state_path,
                                &attachment.path,
                            );
                        }
                    }
                }
                _ => {
                    failed.insert(item.group);
                }
            }
        }
        ensure!(failed.is_empty(), "Some group deliveries remain pending");
        Ok(())
    }

    /// Create the standalone control group once; an uncertain creation is never retried.
    async fn ensure_control_group(&mut self) {
        if !self.config.control_chat {
            return;
        }
        let (has_group, creating) = match self.state.mappings.get(CONTROL_KEY) {
            Some(mapping) if mapping.control => (mapping.group_id_hex.is_some(), mapping.creating),
            _ => return,
        };
        if has_group || creating {
            return;
        }
        let name = "codr control";
        let description =
            "Owner-only codr control chat. Send /new to start a project, /help for commands.";
        let capabilities = match self
            .marmot
            .marmot(json!({"type":"group_create_capabilities"}))
            .await
        {
            Ok(value) => value,
            Err(_) => return,
        };
        if capabilities["type"] != "group_create_capabilities"
            || capabilities["initial_admins"] != true
        {
            eprintln_throttled!("Control chat needs the current connector; no group was created.");
            return;
        }
        {
            let mapping = self.state.mappings.get_mut(CONTROL_KEY).unwrap();
            mapping.creating = true;
            mapping.group_name = Some(name.into());
            mapping.group_description = Some(description.into());
        }
        if self.save().is_err() {
            return;
        }
        let created = self
            .marmot
            .marmot(json!({
                "type": "group_create_with_admins",
                "account_id_hex": self.config.account_id_hex,
                "name": name,
                "members": [self.config.phone_npub],
                "admins": [self.config.phone_npub],
                "description": description,
            }))
            .await;
        match created {
            Ok(response) if response["type"] == "group_created" => {
                let Some(group) = response["group_id_hex"]
                    .as_str()
                    .filter(|group| !group.is_empty())
                else {
                    eprintln_throttled!("Control chat creation needs inspection.");
                    return;
                };
                {
                    let mapping = self.state.mappings.get_mut(CONTROL_KEY).unwrap();
                    mapping.group_id_hex = Some(group.to_owned());
                    mapping.creating = false;
                }
                self.state.enqueue_manual(
                    group,
                    "codr control chat. Send /new (or just new) to start a project, /help for commands.",
                    &format!("control-welcome:{group}"),
                );
                let _ = self.save();
            }
            Ok(_) => eprintln_throttled!("Control chat creation needs inspection."),
            Err(error) => {
                if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() {
                    let mapping = self.state.mappings.get_mut(CONTROL_KEY).unwrap();
                    mapping.creating = false;
                    let _ = self.save();
                } else {
                    eprintln_throttled!(
                        "Control chat creation needs inspection. Use status and resolve-create."
                    );
                }
            }
        }
    }

    pub async fn reconcile(&mut self, agents: Vec<Agent>) -> Result<()> {
        let mut identities = HashSet::new();
        for agent in &agents {
            if agent.agent_session.is_some() {
                ensure!(
                    identities.insert(agent.key()),
                    "Two live agents report the same session identity"
                );
            }
        }
        self.refresh_forwarding()?;
        self.ensure_control_group().await;
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
            let key = agent.key();
            // New native identities get separate groups, even on reused terminals.
            // Keep old saved holds explicit; never repurpose their previous groups.
            if self
                .state
                .mappings
                .get(&key)
                .is_some_and(|m| m.session_change_from.is_some())
            {
                uncertain_terminals.insert(agent.terminal_id.clone());
                continue;
            }
            ensure!(
                current.insert(agent.key(), agent).is_none(),
                "Two live agents report the same session identity"
            );
        }
        self.agents = current;
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
        let keys: Vec<_> = self.agents.keys().cloned().collect();
        // Metadata is independent of message forwarding and transcript checkpoints.
        for key in &keys {
            if let Some(group) = self
                .state
                .mappings
                .get(key)
                .and_then(|m| m.group_id_hex.clone())
            {
                let agent = self.agents[key].clone();
                self.sync_group_profile(key, &agent, &group).await;
            }
        }
        self.activate_forwarding().await?;
        ensure!(
            !self.state.forwarding_resume_pending,
            "Forwarding resume awaits its current history checkpoint"
        );
        for key in keys {
            let agent = self.agents[&key].clone();
            if let Err(error) = self.reconcile_agent(&key, &agent).await {
                if error.downcast_ref::<crate::reads::Pending>().is_none() {
                    eprintln_throttled!(
                        "A session could not reconcile; other sessions remain available."
                    );
                }
            } else {
                let mapping = &self.state.mappings[&key];
                let queued_dispatch = !mapping.prompt_queue.is_empty()
                    && !mapping.pending_prompt
                    && mapping.queue_flight.is_none()
                    && agent.settled();
                if !queued_dispatch {
                    self.reads.finish(&key);
                }
            }
        }
        let ended: Vec<_> = self
            .state
            .mappings
            .iter()
            .filter(|(key, m)| {
                m.active
                    && !m.control
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
                    eprintln_throttled!("Final reply for an ended session remains pending.");
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
            if self.config.auto_forwarding && self.state.mappings[&key].activity.unresolved() {
                forwarding::set(&self.config.state_path, false)?;
                self.refresh_forwarding()?;
                self.state.mappings.get_mut(&key).unwrap().activity.retire();
            }
            let m = self.state.mappings.get_mut(&key).unwrap();
            m.active = false;
            m.pending_prompt = false;
            m.completion_pending = false;
        }
        self.save()?;
        self.process_prompt_queues().await;
        Ok(())
    }

    /// Approve only the replacement still selected on the recorded terminal.
    pub async fn confirm_session_change(&mut self, key: &str) -> Result<()> {
        let mapping = self
            .state
            .mappings
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("Unknown session key"))?;
        ensure!(
            mapping.session_change_from.is_some() && mapping.group_id_hex.is_none(),
            "This session has no pending identity change"
        );
        let expected = mapping
            .agent_record
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Pending session has no terminal identity"))?;
        let agents = herdr::snapshot(&self.herdr).await?;
        ensure!(
            agents.iter().filter(|a| a.key() == key).count() == 1
                && agents.iter().any(|a| a.key() == key
                    && a.terminal_id == expected.terminal_id
                    && a.pane_id == expected.pane_id),
            "Session selection changed; no identity change was approved"
        );
        self.state
            .mappings
            .get_mut(key)
            .unwrap()
            .session_change_from = None;
        self.save()
    }

    async fn seed_native_history(&mut self, key: &str, agent: &Agent) -> Result<bool> {
        if !self.state.mappings[key].output_initialised
            && let Some(_) = agent
                .agent_session
                .as_ref()
                .filter(|session| matches!(session.agent.as_str(), "codex" | "claude"))
        {
            let history = self.read_history(agent).await?;
            let mapping = self.state.mappings.get_mut(key).unwrap();
            mapping.activity.baseline(&history.user_inputs);
            mapping
                .final_ids
                .extend(history.finals.into_iter().map(|reply| reply.id));
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
            self.finish_queued_prompt(key, &history);
            self.observe_activity(key, &history)?;
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
                    self.state.mappings.get_mut(key).unwrap().last_received_at = now();
                    if self.config.edit_responses {
                        if agent.agent_status == "working"
                            && agent
                                .agent_session
                                .as_ref()
                                .is_some_and(|session| session.agent == "claude")
                        {
                            // Retain this bubble until idle; an unchanged working poll
                            // must not replace it after forwarding a completed reply.
                            self.state
                                .mappings
                                .get_mut(key)
                                .unwrap()
                                .response_completion_pending = true;
                        } else {
                            self.state.complete_progress(key);
                        }
                    }
                    if !self.state.mappings[key].prompt_queue.is_empty()
                        && self.state.mappings[key].queue_flight.is_none()
                    {
                        self.state.mappings.get_mut(key).unwrap().pending_prompt = false;
                    }
                    if reply
                        .turn
                        .as_ref()
                        .is_none_or(|turn| !self.state.mappings[key].muted_turns.contains(turn))
                    {
                        let source = output::message_key("Agent", &reply.id, &reply.text);
                        if self.state.forwarding_enabled && !self.state.source_known(group, &source)
                        {
                            let text = reply.text.clone();
                            let cwd = std::path::PathBuf::from(&agent.cwd);
                            let spool = crate::outbound_images::spool(&self.config.state_path);
                            let prepared = tokio::task::spawn_blocking(move || {
                                crate::outbound_images::prepare(&text, &cwd, &spool)
                            })
                            .await??;
                            self.state.enqueue_images(
                                group,
                                &prepared.text,
                                prepared.attachments,
                                vec![source],
                            );
                        }
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
                self.config.auto_forwarding
                    || !mapping.output_initialised
                    || mapping.forwarding_baseline_pending
                    || (self.config.edit_responses
                        && (agent.agent_status == "working"
                            || (agent.settled() && mapping.response_id.is_some())))
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
        if self.state.mappings.get(key).is_none_or(|mapping| {
            !mapping.output_initialised && !mapping.creating && mapping.group_id_hex.is_none()
        }) {
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
                let history = self.read_history(agent).await?;
                m.activity.baseline(&history.user_inputs);
                m.final_ids = history.finals.into_iter().map(|f| f.id).collect();
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
            let kind = agent
                .agent_session
                .as_ref()
                .map(|s| s.agent.as_str())
                .unwrap_or(&agent.agent);
            if self
                .state
                .pending_chat_agents
                .get(&agent.terminal_id)
                .is_some_and(|suffix| suffix == kind)
            {
                self.state.mappings.get_mut(key).unwrap().group_agent_suffix =
                    self.state.pending_chat_agents.remove(&agent.terminal_id);
            }
            self.save()?;
        }
        self.baseline_forwarding(key, agent).await?;
        // Older bridge versions created these groups without a transcript watermark.
        // Seed their existing history once when enabling native reply support.
        self.seed_native_history(key, agent).await?;
        if self.config.auto_forwarding
            && agent
                .agent_session
                .as_ref()
                .is_some_and(|session| output::supported(&session.agent))
        {
            let history = self.read_history(agent).await?;
            self.observe_activity(key, &history)?;
        }
        self.refresh_forwarding()?;
        let name =
            agent.group_name_with_suffix(self.state.mappings[key].group_agent_suffix.as_deref());
        let description = agent.group_description();
        let m = &self.state.mappings[key];
        // Muting suppresses messages, not group invitations or metadata updates.
        if m.group_id_hex.is_none() && !m.creating {
            let capabilities = self
                .marmot
                .marmot(json!({"type":"group_create_capabilities"}))
                .await?;
            ensure!(
                capabilities["type"] == "group_create_capabilities"
                    && capabilities["initial_admins"] == true,
                "The connector must support founding owner-admin rights before creating groups"
            );
            self.refresh_forwarding()?;
            self.state.mappings.get_mut(key).unwrap().creating = true;
            self.save()?;
            // No blind retries: GroupCreate has no idempotency key, even after a timeout.
            match self.marmot.marmot(json!({"type":"group_create_with_admins", "account_id_hex":self.config.account_id_hex,"name":name,"members":[self.config.phone_npub],"admins":[self.config.phone_npub],"description":description})).await {
                    Ok(response) => {
                        ensure!(response["type"] == "group_created", "Unexpected group creation response");
                        let group = response["group_id_hex"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("Missing group ID"))?.to_owned();
                        let m = self.state.mappings.get_mut(key).unwrap(); m.group_id_hex = Some(group.clone()); m.creating = false;
                        m.group_name = Some(name.clone()); m.group_description = Some(description.clone());
                        self.state.enqueue(&group, &format!("Hi! This is your chat with {name}.\n\nSend /help in any Codr chat to see available commands."));
                        self.save()?;
                    }
                    Err(error) => {
                        if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() {
                            self.state.mappings.get_mut(key).unwrap().creating = false; self.save()?;
                        } else { eprintln_throttled!("Group creation needs inspection. Use status and resolve-create before retrying."); }
                        return Ok(());
                    }
                }
        }
        let Some(group) = self.state.mappings[key].group_id_hex.clone() else {
            return Ok(());
        };
        if agent.settled() && self.state.mappings[key].response_completion_pending {
            self.state.complete_progress(key);
        }
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
                    self.state
                        .update_response(key, &group, "> working...", false);
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
            let status = if agent.agent_status == "working" {
                "working..."
            } else {
                &agent.agent_status
            };
            self.state.enqueue(&group, &format!("> {status}"));
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
            && (agent.agent_status == "working"
                || (agent.settled() && self.state.mappings[key].response_id.is_some()))
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
                Err(_) => {
                    eprintln_throttled!("Task list unavailable; response delivery remains active.")
                }
            }
        }
        let opencode = agent
            .agent_session
            .as_ref()
            .is_some_and(|session| session.agent == "opencode");
        if agent.agent_status != "blocked" {
            self.state.mappings.get_mut(key).unwrap().permission = None;
        }
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
                    .map(|snapshot| (snapshot.decision.clone(), snapshot.permission.clone()));
                self.state
                    .mappings
                    .get_mut(key)
                    .unwrap()
                    .decision_checked_at = now();
                self.state
                    .mappings
                    .get_mut(key)
                    .unwrap()
                    .decision_refresh_pending = !matches!(&decision, Ok((Some(_), None)));
                if matches!(&decision, Ok((_, None))) {
                    self.state.mappings.get_mut(key).unwrap().permission = None;
                }
                match decision {
                    Ok((_, Some(permission))) => {
                        self.announce_permission(key, &group, permission)?;
                    }
                    Ok((Some(decision), None)) => {
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
        } else if self.state.forwarding_enabled
            && agent.agent_status == "blocked"
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
        // Claude stays "working" while background tasks run after an end_turn reply.
        // Its transcript only marks finished replies final, so collect them meanwhile.
        let background_claude = agent.agent_status == "working"
            && agent
                .agent_session
                .as_ref()
                .is_some_and(|session| session.agent == "claude");
        if (agent.settled() || background_claude)
            && (self.state.mappings[key].completion_pending
                || background_claude
                || now().saturating_sub(self.state.mappings[key].last_export_at) >= 30)
        {
            self.collect_finals(key, agent, &group).await?;
            if self.config.edit_responses && agent.settled() {
                // Runtime idle can precede the native final. Keep progress and its
                // checklist intact until collect_finals completes the response.
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
                let awaiting_final = (self.state.mappings[key].queue_flight.is_some()
                    || (!self.state.mappings[key].prompt_queue.is_empty()
                        && self.state.mappings[key].pending_prompt)
                    || (self.config.edit_responses
                        && self.state.mappings[key].response_id.is_some()))
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

    fn observe_activity(&mut self, key: &str, history: &output::SessionHistory) -> Result<()> {
        if !self.config.auto_forwarding {
            return Ok(());
        }
        let previous = self.state.mappings[key].activity.clone();
        if self
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .activity
            .observe(&history.user_inputs)
        {
            if let Err(error) = forwarding::set(&self.config.state_path, false) {
                self.state.mappings.get_mut(key).unwrap().activity = previous;
                // Keep outgoing delivery closed even if the control write failed.
                self.state.forwarding_enabled = false;
                return Err(error);
            }
            self.refresh_forwarding()?;
            eprintln_throttled!(
                "Local native user input muted White Noise forwarding for all groups."
            );
        }
        self.save()
    }

    fn activity_unresolved(&self) -> bool {
        !forwarding::load(&self.config.state_path)
            .is_ok_and(|control| control.mode == forwarding::Mode::On)
            && self.config.auto_forwarding
            && self
                .state
                .mappings
                .values()
                .any(|mapping| mapping.activity.unresolved())
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
        // Preserve unknown-session terminals from the authoritative herdr snapshot.
        self.reads.scheduling = false;
        let result = self.reconcile(self.last_snapshot.clone()).await;
        self.reads.scheduling = true;
        result
    }

    pub async fn inbound(&mut self, event: &Value) -> Result<()> {
        self.route_inbound(event, false, false).await
    }

    fn finish_queued_prompt(&mut self, key: &str, history: &output::SessionHistory) {
        let mapping = self.state.mappings.get_mut(key).unwrap();
        if mapping.queue_flight.as_ref().is_some_and(|flight| {
            flight.confirmed
                && history
                    .finals
                    .last()
                    .is_some_and(|reply| Some(&reply.id) != flight.before_final.as_ref())
        }) {
            mapping.queue_flight = None;
            mapping.pending_prompt = false;
        }
    }

    async fn process_prompt_queues(&mut self) {
        let keys: Vec<_> = self.agents.keys().cloned().collect();
        for key in keys {
            if self.refresh_forwarding().is_err()
                || !self.herdr_available
                || self.state.forwarding_resume_pending
            {
                return;
            }
            let Some(mapping) = self.state.mappings.get(&key) else {
                continue;
            };
            let index = mapping
                .prompt_queue
                .iter()
                .position(|prompt| prompt.media.is_some())
                .unwrap_or(0);
            let Some(prompt) = mapping.prompt_queue.get(index).cloned() else {
                continue;
            };
            let media = prompt.media.is_some();
            let prepared = prompt
                .media
                .as_ref()
                .is_some_and(|message| message["_codr_prepared_text"].is_string());
            if mapping.group_id_hex.as_deref() != Some(&prompt.group)
                || (!media
                    && (mapping.pending_prompt
                        || mapping.queue_flight.is_some()
                        || !self.agents[&key].settled()
                        || !self.state.forwarding_enabled))
                || (!prepared
                    && self
                        .state
                        .media_pending
                        .iter()
                        .any(|event| event["group_id_hex"] == prompt.group))
            {
                self.reads.finish(&key);
                continue;
            }
            if let Some(message) = &prompt.media
                && !message["_codr_prepared_text"].is_string()
            {
                if self.state.media_pending.len() < 16 {
                    self.state.media_pending.push(
                        json!({"marmot_agent_control":PROTOCOL,"type":"inbound_message",
                        "account_id_hex":self.config.account_id_hex,"group_id_hex":prompt.group,
                        "message":message,"_codr_queued_image":true}),
                    );
                    if self.save().is_err() {
                        self.state.media_pending.pop();
                    }
                }
                self.reads.finish(&key);
                continue;
            }
            let event = json!({"marmot_agent_control":PROTOCOL,"type":"inbound_message","account_id_hex":self.config.account_id_hex,
                "group_id_hex":prompt.group,"message":{"message_id_hex":prompt.id,"sender":{"account_id_hex":self.phone},"text":prompt.prompt_text()}});
            if let Err(error) =
                Box::pin(self.route_inbound(&event, prompt.media.is_some(), true)).await
                && !error.is::<crate::reads::Pending>()
            {
                eprintln_throttled!("A queued prompt remains paused; other chats can continue.");
            }
            self.reads.finish(&key);
        }
    }

    /// Recover an old installation's last own message without treating the incoming
    /// /sync request (or another participant's message) as a new send reference.
    async fn last_own_message_at(&self, group: &str, requested_at: u64) -> Result<Option<u64>> {
        let mut before: Option<(u64, String)> = None;
        let mut cursors = HashSet::new();
        let mut latest = None;
        loop {
            let mut request = json!({"type":"timeline_list","account_id_hex":self.config.account_id_hex,"group_id_hex":group,"limit":50});
            if let Some((at, id)) = &before {
                request["before"] = json!({"recorded_at":at,"message_id_hex":id});
            }
            let page = self.marmot.marmot(request).await?;
            ensure!(page["type"] == "timeline_page", "Invalid timeline response");
            let rows = page["messages"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing timeline messages"))?;
            let page_latest = rows
                .iter()
                .filter(|row| {
                    row["sender"]["is_self"] == true
                        && row["kind"] == 9
                        && row["availability"] == "available"
                })
                .filter_map(|row| row["recorded_at"].as_u64())
                .filter(|at| *at > 0 && *at <= requested_at)
                .max();
            latest = latest.max(page_latest);
            if rows.is_empty() || page["has_more_before"] != true {
                return Ok(latest);
            }
            let cursor = (
                rows[0]["recorded_at"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("Invalid timeline time"))?,
                rows[0]["message_id_hex"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing timeline message ID"))?
                    .to_owned(),
            );
            ensure!(
                cursors.insert(cursor.1.clone()),
                "Timeline cursor did not advance"
            );
            before = Some(cursor);
        }
    }

    /// White Noise-only commands bypass agent input and the global phone-wake path.
    async fn chat_command(
        &mut self,
        key: &str,
        group: &str,
        id: &str,
        recent_window: Option<u64>,
    ) -> Result<()> {
        use sha2::{Digest, Sha256};
        self.refresh_forwarding()?;
        let request = hex::encode(Sha256::digest(serde_json::to_vec(&(group, id))?));
        let mut replies = Vec::new();
        let mut unavailable = 0;
        let requested_at = now();
        let mut reference = self.state.last_confirmed_send_at;
        if let Some(window) = recent_window {
            if reference.is_none() {
                let groups: HashSet<_> = self
                    .state
                    .mappings
                    .values()
                    .filter_map(|mapping| mapping.group_id_hex.clone())
                    .collect();
                for target in groups {
                    match self.last_own_message_at(&target, requested_at).await {
                        Ok(at) => reference = reference.max(at),
                        Err(_) => unavailable += 1,
                    }
                }
            }
            let cutoff = requested_at.saturating_sub(window);
            let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
            let mut readers = tokio::task::JoinSet::new();
            for (mapping_key, mapping) in &self.state.mappings {
                let Some(target) = mapping.group_id_hex.clone() else {
                    continue;
                };
                let Some(agent) = self
                    .agents
                    .get(mapping_key)
                    .or(mapping.agent_record.as_ref())
                    .filter(|agent| agent.key() == *mapping_key)
                else {
                    continue;
                };
                let Some(session) = agent
                    .agent_session
                    .clone()
                    .filter(|session| output::supported(&session.agent))
                else {
                    continue;
                };
                let config = self.config.clone();
                let limit = limit.clone();
                readers.spawn(async move {
                    let _permit = limit.acquire_owned().await?;
                    Ok::<_, anyhow::Error>((target, output::read_history(&config, &session).await))
                });
            }
            while let Some(result) = readers.join_next().await {
                match result {
                    Ok(Ok((target, Ok(mut history)))) => {
                        history
                            .recent_messages
                            .retain(|message| !self.state.source_known(&target, &message.key()));
                        if history
                            .activity_at
                            .is_some_and(|at| at <= now() && at >= cutoff)
                            && let Some(excerpt) = history.catch_up()
                        {
                            let messages = history
                                .recent_messages
                                .iter()
                                .map(|message| message.key())
                                .collect();
                            replies.push((target, excerpt, messages));
                        }
                    }
                    _ => unavailable += 1,
                }
            }
        }
        // Persist the complete batch and consumption together before any network send.
        let before = self.state.clone();
        if self.state.last_confirmed_send_at.is_none() {
            self.state.last_confirmed_send_at = reference;
        }
        let count = replies.len();
        for (target, text, messages) in replies {
            self.state.enqueue_tracked(
                &target,
                &text,
                Some(&format!("sync:{request}:{target}")),
                messages,
            );
        }
        let response = if recent_window.is_some() {
            let mut status = if count == 0 {
                "Recent: nothing new".into()
            } else {
                format!(
                    "Recent: {count} {} queued",
                    if count == 1 { "chat" } else { "chats" }
                )
            };
            if unavailable > 0 {
                status.push_str(&format!("; {unavailable} unavailable"));
            }
            status.push('.');
            status
        } else {
            format!(
                "# codr commands\n\n### General\n\n**`/help`**  \nShow commands\n\n**`/status`**  \nConnection, state, queue and unmute\n\n**`/last [count]`**  \nReplay the newest message in this chat without unmuting. Use /last4 or /last 4 for four; count 1–100.\n\n**`/recent [hours]`**  \nGet newest 3 messages per chat active in the past hour. Use /recent2 or /recent 2 for two hours.\n\n**`/update`**  \nCheck ngit for a stable bridge update; confirm before installing\n\n### Projects & Agents\n\n**`/new`**  \nSet up a project\n\n**`/agent`**  \nChoose agent (separate chat)\n\n**`/model`**  \nChoose model\n\n### Tasks\n\n**`/queue`**  \nView pending deliveries\n\n**`/cancel`**  \nInterrupt the current task\n\n### Sync\n\n**`/unmute`**  \nUnmute all chats and return to automatic sync\n\n**`/sync auto|on|off`**  \nSet persistent sync mode\nCurrent sync mode: {}\n\nCommands accept unambiguous prefixes (/n, /st, /sy). In the control chat the leading / is optional.",
                forwarding::load(&self.config.state_path)?.mode.label()
            )
        };
        self.state
            .enqueue_manual(group, &response, &format!("command:{request}"));
        self.state
            .mappings
            .get_mut(key)
            .unwrap()
            .seen_messages
            .push(id.into());
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        Ok(())
    }

    async fn last_command(&mut self, key: &str, group: &str, id: &str, count: usize) -> Result<()> {
        let session = (!self.state.mappings[key].control)
            .then(|| {
                self.agents
                    .get(key)
                    .or(self.state.mappings[key].agent_record.as_ref())
            })
            .flatten()
            .filter(|agent| agent.key() == key)
            .and_then(|agent| agent.agent_session.clone())
            .filter(|session| output::supported(&session.agent));
        let response = if let Some(session) = session {
            match output::read_history(&self.config, &session).await {
                Ok(history) => history
                    .last(count)
                    .unwrap_or_else(|| "No conversation messages yet.".into()),
                Err(_) => "Chat history is unavailable. Try /last again.".into(),
            }
        } else {
            "Use /last in an agent chat with a saved session.".into()
        };
        // Explicit replays bypass mute, but must not consume automatic delivery keys.
        let before = self.state.clone();
        self.state
            .enqueue_manual(group, &response, &format!("last:{group}:{id}"));
        self.state
            .mappings
            .get_mut(key)
            .unwrap()
            .seen_messages
            .push(id.into());
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        Ok(())
    }

    async fn control_message(
        &mut self,
        key: &str,
        group: &str,
        id: &str,
        text: &str,
    ) -> Result<()> {
        let trimmed = text.trim();
        let response = if trimmed.eq_ignore_ascii_case("/status") {
            let sessions = self
                .state
                .mappings
                .values()
                .filter(|mapping| !mapping.control && mapping.group_id_hex.is_some())
                .count();
            let control = forwarding::load(&self.config.state_path)?;
            format!(
                "codr control chat. {sessions} mapped session(s). Forwarding: {}. Use /new to start a project, /help for commands.",
                control.mode.label()
            )
        } else if trimmed.eq_ignore_ascii_case("/unmute") || trimmed.eq_ignore_ascii_case("unmute")
        {
            forwarding::set_mode(&self.config.state_path, forwarding::Mode::Auto)?;
            self.refresh_forwarding()?;
            "White Noise forwarding is enabled for all chats. No prompt was sent.".into()
        } else if trimmed.is_empty() {
            "Send /new to start a project, /help for commands.".into()
        } else {
            "This is the codr control chat, not a session. Use /new (or just new) to start a project, /help for commands. To prompt an agent, open its chat.".into()
        };
        crate::launch::reply(self, key, group, id, &response)
    }

    async fn cancel_task(&mut self, key: &str, group: &str, id: &str, plain: bool) -> Result<()> {
        if !plain {
            return crate::launch::reply(
                self,
                key,
                group,
                id,
                "Send /cancel as complete standalone text without attachments or a voice transcript.",
            );
        }
        // Reset routing even if the runtime is idle, ended or unreachable. Keep
        // uncertain external operations and accepted deliveries for reconciliation.
        let before_reset = self.state.clone();
        let mapping = self.state.mappings.get_mut(key).unwrap();
        mapping.decision = None;
        mapping.decision_checked_at = 0;
        mapping.decision_refresh_pending = false;
        mapping.permission = None;
        mapping.update_offer = None;
        if mapping.launch_draft.as_ref().is_some_and(|draft| {
            !matches!(
                draft.step,
                crate::launch_flow::Step::Applying | crate::launch_flow::Step::Uncertain
            )
        }) {
            mapping.launch_draft = None;
        }
        if let Some(request) = mapping.launch.as_mut()
            && request.phase == crate::launch::Phase::Awaiting
        {
            request.phase = crate::launch::Phase::Cancelled;
        }
        if let Err(error) = self.save() {
            self.state = before_reset;
            return Err(error);
        }
        if self.state.mappings[key].control {
            return crate::launch::reply(
                self,
                key,
                group,
                id,
                "Pending questions and menus cleared in this chat. The control chat has no task to interrupt. History and accepted deliveries were kept.",
            );
        }
        let record = self
            .agents
            .get(key)
            .or(self.state.mappings[key].agent_record.as_ref())
            .cloned();
        let Some(record) = record else {
            return crate::launch::reply(
                self,
                key,
                group,
                id,
                "Pending questions and menus cleared. This session has ended; no interrupt was sent. History and accepted deliveries were kept.",
            );
        };
        let current = herdr::get(&self.herdr, &record).await?;
        ensure!(
            current.key() == key,
            "The mapped session changed; no interrupt was sent"
        );
        if current.settled() {
            return crate::launch::reply(
                self,
                key,
                group,
                id,
                "Pending questions and menus cleared. No task is running in this chat; no interrupt was sent. History and accepted deliveries were kept.",
            );
        }
        ensure!(
            matches!(current.agent_status.as_str(), "working" | "blocked"),
            "Agent state is unavailable; no interrupt was sent"
        );
        let before = self.state.clone();
        self.state
            .mappings
            .get_mut(key)
            .unwrap()
            .seen_messages
            .push(id.into());
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        let response = self
            .herdr
            .herdr("agent.interrupt", json!({"target":current.pane_id}))
            .await;
        let message = match response {
            Ok(response) if response["result"]["type"] == "ok" => {
                "Pending questions and menus cleared. Interrupt sent to the current task. Accepted deliveries and chat history were kept."
            }
            Err(error) if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() => {
                self.state = before;
                self.save()?;
                return Err(error);
            }
            _ => {
                "Pending questions and menus cleared, but the interrupt could not be confirmed. Inspect the agent before trying again. No automatic retry will run."
            }
        };
        self.state
            .enqueue_manual(group, message, &format!("cancel:{group}:{id}"));
        self.save()
    }

    async fn route_inbound(&mut self, event: &Value, prepared: bool, queued: bool) -> Result<()> {
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
        let queued_index = if queued && prepared {
            self.state.mappings[&key]
                .prompt_queue
                .iter()
                .position(|prompt| prompt.media.is_some())
                .unwrap_or(0)
        } else {
            0
        };
        if queued {
            ensure!(
                self.state.mappings[&key]
                    .prompt_queue
                    .get(queued_index)
                    .is_some_and(|prompt| prompt.id == id
                        && prompt.group == group
                        && msg["text"].as_str() == Some(prompt.prompt_text())),
                "Queued prompt identity changed"
            );
        }
        if !queued
            && !prepared
            && let Some(prompt) = self
                .state
                .mappings
                .get_mut(&key)
                .unwrap()
                .prompt_queue
                .iter_mut()
                .find(|prompt| prompt.id == id && prompt.group == group && prompt.media.is_some())
        {
            if !crate::media::complete_refs(prompt.media.as_ref().unwrap())
                && crate::media::complete_refs(msg)
            {
                prompt.media = Some(msg.clone());
                if let Some(pending) = self.state.media_pending.iter_mut().find(|pending| {
                    pending["group_id_hex"] == group && pending["message"]["message_id_hex"] == id
                }) {
                    pending["message"] = msg.clone();
                    pending["_codr_retry_at"] = json!(0);
                }
                self.save()?;
            }
            return Ok(());
        }
        if !queued
            && self.state.mappings[&key]
                .seen_messages
                .iter()
                .any(|seen| seen == id)
        {
            return Ok(());
        }
        // Whole-second transport timestamps need a later second, not a same-second tie.
        // Bound clock skew so a bad sender timestamp cannot stall control replies.
        if let Some(at) = msg["recorded_at"]
            .as_u64()
            .filter(|at| *at <= now().saturating_add(5))
        {
            let mapping = self.state.mappings.get_mut(&key).unwrap();
            mapping.manual_reply_after = mapping.manual_reply_after.max(at.saturating_add(1));
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
        let raw_text = msg["text"].as_str().unwrap_or("");
        let control = self.state.mappings[&key].control;
        let menu_open = self.state.mappings[&key]
            .launch_draft
            .as_ref()
            .is_some_and(|draft| now() < draft.expires_at)
            || self.state.mappings[&key]
                .launch
                .as_ref()
                .is_some_and(|request| {
                    request.phase == crate::launch::Phase::Awaiting && now() < request.expires_at
                });
        let plain_command = !prepared
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["attachments"]
                .as_array()
                .is_none_or(|items| items.is_empty())
            && msg["media"].as_array().is_none_or(|items| items.is_empty());
        let normalized = if plain_command && !queued {
            crate::commands::normalize(raw_text, control, menu_open)
        } else {
            crate::commands::Normalized::Text(raw_text.to_owned())
        };
        let text = match &normalized {
            crate::commands::Normalized::Text(value) => value.as_str(),
            crate::commands::Normalized::Ambiguous(options) if control => {
                let options = options
                    .iter()
                    .map(|command| format!("/{command}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    &format!("Ambiguous command. Did you mean {options}? Add another letter."),
                );
            }
            _ => raw_text,
        };
        if !prepared && !queued && text.trim().eq_ignore_ascii_case("/cancel") {
            return self.cancel_task(&key, group, id, plain_command).await;
        }
        if !prepared
            && !queued
            && text.split_whitespace().next().is_some_and(|command| {
                command
                    .to_ascii_lowercase()
                    .strip_prefix("/last")
                    .is_some_and(|suffix| suffix.bytes().all(|byte| byte.is_ascii_digit()))
            })
        {
            self.refresh_forwarding()?;
            if !plain_command {
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "Send /last as complete standalone text without attachments or a voice transcript.",
                );
            }
            let args: Vec<_> = text.split_whitespace().collect();
            let count = match args.as_slice() {
                [_] => Some(1),
                [_, count] if count.bytes().all(|byte| byte.is_ascii_digit()) => {
                    count.parse::<usize>().ok()
                }
                _ => None,
            }
            .filter(|count| (1..=output::MAX_LAST_MESSAGES).contains(count));
            let Some(count) = count else {
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "Use /last for the newest message, or /last 4 (also /last4) for four. Count must be a whole number from 1 to 100. Sync stays unchanged.",
                );
            };
            return self.last_command(&key, group, id, count).await;
        }
        let status_command = plain_command && text == "/status";
        let immediate_prompt =
            (!queued && plain_command || prepared) && !status_command && !text.trim().is_empty();
        if !prepared
            && !queued
            && text
                .split_whitespace()
                .next()
                .is_some_and(|command| command.eq_ignore_ascii_case("/update"))
        {
            return crate::update::handle(
                self,
                &key,
                group,
                id,
                text,
                plain_command,
                msg["recorded_at"].as_u64(),
            )
            .await;
        }
        if !prepared
            && !queued
            && text
                .split_whitespace()
                .next()
                .is_some_and(|command| command.eq_ignore_ascii_case("/sync"))
        {
            if !plain_command {
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "Send /sync as complete standalone text without attachments or a voice transcript.",
                );
            }
            let args: Vec<_> = text.split_whitespace().collect();
            let mode = match args.as_slice() {
                [_] => None,
                [_, mode] if mode.eq_ignore_ascii_case("auto") => Some(forwarding::Mode::Auto),
                [_, mode] if mode.eq_ignore_ascii_case("on") => Some(forwarding::Mode::On),
                [_, mode] if mode.eq_ignore_ascii_case("off") => Some(forwarding::Mode::Off),
                _ => {
                    return crate::launch::reply(
                        self,
                        &key,
                        group,
                        id,
                        "Use /sync auto, /sync on, or /sync off. /unmute clears the override.",
                    );
                }
            };
            let control = if let Some(mode) = mode {
                forwarding::set_mode(&self.config.state_path, mode)?
            } else {
                forwarding::load(&self.config.state_path)?
            };
            self.refresh_forwarding()?;
            return crate::launch::reply(
                self,
                &key,
                group,
                id,
                &format!(
                    "Current sync mode: {}.\n/sync auto — Automatic\n/sync on — Always on\n/sync off — Always off\n/unmute — Clear override and resume automatic sync",
                    control.mode.label()
                ),
            );
        }
        if plain_command
            && (text.trim().eq_ignore_ascii_case("/unmute")
                || text.trim().eq_ignore_ascii_case("unmute"))
        {
            forwarding::set_mode(&self.config.state_path, forwarding::Mode::Auto)?;
        }
        if !prepared
            && !queued
            && (crate::launch_flow::is_command(text)
                || (!text.trim().is_empty()
                    && !text.trim().starts_with('/')
                    && self.state.mappings[&key]
                        .launch
                        .as_ref()
                        .is_some_and(|request| request.phase == crate::launch::Phase::Awaiting))
                || self.state.mappings[&key]
                    .launch_draft
                    .as_ref()
                    .is_some_and(|draft| crate::launch_flow::short_reply(draft, text).is_some()))
        {
            self.refresh_forwarding()?;
            return crate::launch_flow::handle(self, &key, group, id, text, plain_command).await;
        }
        if !prepared
            && !crate::media::images(msg)
            && output::permission::is_reply(text, self.state.mappings[&key].permission.is_some())
        {
            ensure!(!queued, "Permission replies cannot be queued");
            return self
                .reply_permission(&key, group, id, text, plain_command)
                .await;
        }
        if plain_command
            && (text.trim().eq_ignore_ascii_case("/queue")
                || text.trim().eq_ignore_ascii_case("/clear-queue"))
        {
            self.refresh_forwarding()?;
            let before = self.state.clone();
            let mapping = self.state.mappings.get_mut(&key).unwrap();
            let count = mapping.prompt_queue.len();
            let response = if text.trim().eq_ignore_ascii_case("/clear-queue") {
                "/clear-queue has been retired. Phone text is sent directly. Use /cancel to interrupt the current task. Existing accepted deliveries were kept.".into()
            } else {
                format!(
                    "Pending deliveries: {count}, {}. New phone text is handed directly to the agent. Media preparation and previously accepted deliveries are preserved.",
                    if mapping
                        .queue_flight
                        .as_ref()
                        .is_some_and(|flight| !flight.confirmed)
                    {
                        "paused: submission is uncertain; inspect the terminal and use local clear-prompt"
                    } else if mapping.pending_prompt || mapping.queue_flight.is_some() {
                        "a prompt is in flight"
                    } else {
                        "no prompt in flight"
                    }
                )
            };
            mapping.seen_messages.push(id.into());
            self.state
                .enqueue_manual(group, &response, &format!("queue-control:{group}:{id}"));
            if let Err(error) = self.save() {
                self.state = before;
                return Err(error);
            }
            return Ok(());
        }
        if plain_command
            && text
                .split_whitespace()
                .next()
                .is_some_and(|command| command.eq_ignore_ascii_case("/recent"))
        {
            let args: Vec<_> = text.split_whitespace().collect();
            let hours = match args.as_slice() {
                [_] => Some(1_u64),
                [_, hours] if hours.bytes().all(|byte| byte.is_ascii_digit()) => {
                    hours.parse::<u64>().ok()
                }
                _ => None,
            };
            let window = hours
                .filter(|hours| *hours > 0)
                .and_then(|hours| hours.checked_mul(3600));
            let Some(window) = window else {
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "Use /recent for the past hour, or /recent 2 (also /recent2) for the past two hours. Hours must be a positive whole number; the value is invalid or too large. Each chat still gets at most three messages.",
                );
            };
            return self.chat_command(&key, group, id, Some(window)).await;
        }
        if plain_command && text.trim().eq_ignore_ascii_case("/help") {
            return self.chat_command(&key, group, id, None).await;
        }
        if self.state.mappings[&key].control {
            if queued || prepared || !plain_command {
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "The control chat accepts complete standalone text commands only.",
                );
            }
            if menu_open {
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "A setup menu is open. Reply with a listed number, 0 to cancel, or /new to restart.",
                );
            }
            return self.control_message(&key, group, id, text).await;
        }
        self.refresh_forwarding()?;
        if !queued {
            forwarding::wake(&self.config.state_path, group, id)?;
        }
        self.refresh_forwarding()?;
        if queued && !prepared {
            ensure!(
                self.state.forwarding_enabled && !self.state.forwarding_resume_pending,
                "Queue paused by forwarding controls"
            );
        }
        ensure!(
            self.herdr_available,
            "herdr unavailable; inbound message deferred until recovery"
        );
        self.activate_forwarding().await?;
        ensure!(
            !self.state.forwarding_resume_pending,
            "Forwarding resume awaits its current history checkpoint"
        );
        if !prepared
            && (text.trim().eq_ignore_ascii_case("unmute")
                || text.trim().eq_ignore_ascii_case("/unmute"))
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["attachments"]
                .as_array()
                .is_none_or(|items| items.is_empty())
            && msg["media"].as_array().is_none_or(|items| items.is_empty())
        {
            // The shared phone-wake path already resumed all chats. Consume this
            // command before any agent readiness, prompt, or decision handling.
            self.state
                .mappings
                .get_mut(&key)
                .unwrap()
                .seen_messages
                .push(id.into());
            self.state.enqueue(
                group,
                "White Noise forwarding is enabled for all chats. No prompt was sent.",
            );
            return self.save();
        }
        ensure!(
            self.agents.contains_key(&key) || !self.state.mappings[&key].active,
            "Session identity is uncertain; phone input deferred"
        );
        if let Some(agent) = self.agents.get(&key).cloned()
            && (self.state.forwarding_enabled || !immediate_prompt)
        {
            self.baseline_forwarding(&key, &agent).await?;
        }
        if !queued
            && !status_command
            && !text.trim().is_empty()
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && (prepared
                || msg["attachments"]
                    .as_array()
                    .is_none_or(|items| items.is_empty()))
            && msg["media"].as_array().is_none_or(|media| media.is_empty())
            && let Some(agent) = self.agents.get(&key).cloned()
            && self
                .answer_decision(&key, &agent, group, id, text, !prepared)
                .await?
        {
            return Ok(());
        }
        // A final transcript record can lag herdr's idle state. Resolve it before
        // opening another response, so it cannot overwrite the next prompt's message.
        if self.config.edit_responses
            && !status_command
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
        let queue_image = !prepared
            && crate::media::images(msg)
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true;
        if !queued && !prepared && queue_image && self.agents.contains_key(&key) {
            let before = self.state.clone();
            let mapping = self.state.mappings.get_mut(&key).unwrap();
            let accepted = mapping.prompt_queue.len() < 16 && text.len() <= 30000;
            let response: String = if !accepted {
                "Media preparation limit reached (16 accepted deliveries, 30000 UTF-8 bytes per caption). No image was accepted.".into()
            } else {
                mapping.prompt_queue.push(crate::state::QueuedPrompt {
                    id: id.into(),
                    group: group.into(),
                    text: text.into(),
                    media: queue_image.then(|| msg.clone()),
                });
                "Preparing image. It will be delivered as soon as the download is ready.".into()
            };
            mapping.seen_messages.push(id.into());
            self.state
                .enqueue_manual(group, &response, &format!("queued:{group}:{id}"));
            if let Err(error) = self.save() {
                self.state = before;
                return Err(error);
            }
            return Ok(());
        }
        if !status_command
            && !text.trim().is_empty()
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["media"].as_array().is_none_or(|a| a.is_empty())
            && self.agents.get(&key).is_some_and(Agent::settled)
            && !self.state.mappings[&key].pending_prompt
        {
            let agent = self.agents[&key].clone();
            match herdr::get(&self.herdr, &agent).await {
                Ok(current) if current.key() == key && (current.settled() || immediate_prompt) => {}
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
        if !status_command
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["media"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            && self.state.media_pending.len() < 16
            && self.agents.contains_key(&key)
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
                        "Preparing attachment. It will be delivered as soon as the download is ready."
                    },
                );
            }
            self.state.media_pending.push(pending);
            return self.save();
        }
        let phone_history = if (self.config.auto_forwarding || queued)
            && (!immediate_prompt || self.state.forwarding_enabled)
            && !status_command
            && !text.trim().is_empty()
            && msg["text_truncated"] != true
            && msg["attachments_truncated"] != true
            && msg["media"].as_array().is_none_or(|media| media.is_empty())
            && !media_busy
            && !self.state.mappings[&key].pending_prompt
            && self.agents.get(&key).is_some_and(|agent| {
                agent.settled()
                    && agent
                        .agent_session
                        .as_ref()
                        .is_some_and(|session| output::supported(&session.agent))
            }) {
            let revision = self.state.forwarding_revision.clone();
            let session = self.agents[&key].agent_session.as_ref().unwrap().clone();
            let history = if queued && self.reads.background {
                let agent = self.agents[&key].clone();
                self.read_history(&agent).await?
            } else {
                self.reads.fresh(&self.config, &session).await?.history
            };
            self.refresh_forwarding()?;
            ensure!(
                self.state.forwarding_enabled && self.state.forwarding_revision == revision,
                "Forwarding selection changed; phone input deferred before submission"
            );
            Some(history)
        } else {
            None
        };
        if immediate_prompt && let Some(agent) = self.agents.get(&key) {
            let current = herdr::get(&self.herdr, agent).await?;
            ensure!(
                current.key() == key,
                "The mapped session changed; phone input was not submitted"
            );
            if current.agent_status == "blocked" {
                if prepared {
                    anyhow::bail!("Prepared media awaits resolution of the native blocking dialog");
                }
                return crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "The agent is waiting for interactive input and cannot accept a prompt. Answer its decision or use /cancel. No prompt was queued.",
                );
            }
        }
        // Persist consumption before terminal input: uncertain prompts must not run twice.
        let before_consumption = (queued || prepared).then(|| self.state.clone());
        let queued_prompt = if queued {
            if !prepared {
                ensure!(
                    !media_busy
                        && !self.state.mappings[&key].pending_prompt
                        && self.state.mappings[&key].queue_flight.is_none()
                        && self.agents.get(&key).is_some_and(Agent::settled),
                    "Queued prompt is not ready"
                );
                ensure!(
                    phone_history.is_some(),
                    "Queue requires a supported transcript reader"
                );
                let mapping = self.state.mappings.get_mut(&key).unwrap();
                mapping.queue_flight = Some(crate::state::QueueFlight {
                    before_final: phone_history
                        .as_ref()
                        .and_then(|history| history.finals.last())
                        .map(|reply| reply.id.clone()),
                    confirmed: false,
                });
            }
            Some(
                self.state
                    .mappings
                    .get_mut(&key)
                    .unwrap()
                    .prompt_queue
                    .remove(queued_index),
            )
        } else {
            None
        };
        let m = self.state.mappings.get_mut(&key).unwrap();
        if !queued {
            m.seen_messages.push(id.into());
        }
        if !prepared && let Err(error) = self.save() {
            if let Some(before) = before_consumption {
                self.state = before;
            }
            return Err(error);
        }
        let response = if status_command {
            Some(format!(
                "> {} herdr session: {}. {} waiting prompt(s).",
                self.state.mappings[&key].last_agent_status,
                if self.agents.contains_key(&key) {
                    "connected"
                } else {
                    "ended"
                },
                self.state.mappings[&key].prompt_queue.len()
            ))
        } else if msg["text_truncated"] == true || msg["attachments_truncated"] == true {
            Some("This offline message was truncated by MDK. No prompt was sent. Send a shorter text message.".into())
        } else if !msg["media"].as_array().is_none_or(|a| a.is_empty()) {
            Some("Media preparation is unavailable or at capacity. No prompt was sent. Try again when a preparation slot is available.".into())
        } else if let Some(agent) = self.agents.get(&key).cloned() {
            if !immediate_prompt
                && (media_busy || self.state.mappings[&key].pending_prompt || !agent.settled())
            {
                Some("The agent is busy or blocked. Wait for it to become idle before sending another prompt.".into())
            } else if text.trim().is_empty() {
                Some("Send a non-empty text message.".into())
            } else {
                let mapping = self.state.mappings.get_mut(&key).unwrap();
                let was_pending = mapping.pending_prompt;
                let new_flight = immediate_prompt && mapping.queue_flight.is_none();
                if new_flight {
                    mapping.queue_flight = Some(crate::state::QueueFlight {
                        before_final: phone_history
                            .as_ref()
                            .and_then(|history| history.finals.last().map(|reply| reply.id.clone()))
                            .or_else(|| mapping.final_ids.last().cloned()),
                        confirmed: false,
                    });
                }
                mapping.pending_prompt = true;
                if let Some(history) = phone_history {
                    mapping.activity.start_phone(id, text, &history.user_inputs);
                } else if immediate_prompt && self.config.auto_forwarding {
                    mapping.activity.start_phone_current(id, text);
                }
                if let Err(error) = self.save() {
                    if prepared && let Some(before) = before_consumption {
                        self.state = before;
                    }
                    return Err(error);
                }
                match herdr::prompt(&self.herdr, &agent, text).await {
                    Ok(response) if response["result"]["type"]=="agent_prompted" => {
                        if new_flight {
                            let mapping=self.state.mappings.get_mut(&key).unwrap();
                            mapping.queue_flight.as_mut().unwrap().confirmed=true;
                            mapping.completion_pending=true;
                        }
                        if queued && !prepared {
                            ensure!(
                                response["result"]["type"] == "agent_prompted",
                                "Queued prompt acknowledgement was not confirmed"
                            );
                            let mapping = self.state.mappings.get_mut(&key).unwrap();
                            mapping.queue_flight.as_mut().unwrap().confirmed = true;
                            mapping.completion_pending = true;
                        }
                        self.state
                            .mappings
                            .get_mut(&key)
                            .unwrap()
                            .activity
                            .confirm_phone(id);
                        self.save()?;
                        if self.config.edit_responses {
                            if !was_pending && self.state.mappings[&key].response_id.is_some() {
                                self.state.update_response(
                                    &key,
                                    group,
                                    "> idle No completed reply was recorded.",
                                    true,
                                );
                            }
                            if !was_pending || self.state.mappings[&key].response_id.is_none() {self.state.begin_response(&key, group);}
                        }
                        None
                    }
                    Ok(_) => Some("The prompt could not be confirmed. It will not be retried. Inspect the agent before submitting again.".into()),
                    Err(error) => {
                        if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() {
                            let m = self.state.mappings.get_mut(&key).unwrap();
                            m.pending_prompt = was_pending;
                            if new_flight {m.queue_flight=None;}
                            m.activity.cancel_phone(id);
                            if let Some(prompt) = queued_prompt {
                                if !prepared {m.queue_flight = None;}
                                m.prompt_queue.insert(queued_index, prompt);
                            } else {
                                m.seen_messages.retain(|seen| seen != id);
                            }
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
                            let queued_image = event["_codr_queued_image"] == true;
                            if queued_image
                                && let Some(index) =
                                    mapping.prompt_queue.iter().position(|prompt| {
                                        Some(prompt.id.as_str())
                                            == event["message"]["message_id_hex"].as_str()
                                    })
                            {
                                mapping.prompt_queue.remove(index);
                            } else if queued_image {
                                // Cleared waiting images must never become agent input.
                                self.save()?;
                                return Ok(());
                            }
                            if let Some(id) = event["message"]["message_id_hex"].as_str()
                                && !mapping.seen_messages.iter().any(|seen| seen == id)
                            {
                                mapping.seen_messages.push(id.into());
                            }
                            let notice = if crate::media::audio(&event["message"]) {
                                "Audio preparation failed. No prompt was sent. Check the attachment download and local transcription service, then try again."
                            } else {
                                "Attachment preparation failed. No prompt was sent. Check that the file is available and within the four-file, 25 MiB-per-file limits, then try again."
                            };
                            if queued_image {
                                self.state.enqueue_manual(
                                    group,
                                    notice,
                                    &format!(
                                        "media-failed:{group}:{}",
                                        event["message"]["message_id_hex"].as_str().unwrap_or("")
                                    ),
                                );
                            } else {
                                self.state.enqueue(group, notice);
                            }
                        }
                        self.save()?;
                    }
                }
            }
        }
        if let Some(index) = self.state.media_pending.iter().position(|event| {
                event["_codr_prepared_text"].is_string()
                    && event["_codr_retry_at"].as_u64().unwrap_or(0) <= now()
            })
            // An active download owns element zero. Removing a ready later
            // element cannot disturb that job or hold up prepared input.
            && (self.media_job.is_none() || index > 0)
        {
            let mut event = self.state.media_pending[index].clone();
            if event["_codr_queued_image"] == true {
                if let Some(prompt) = self
                    .state
                    .mappings
                    .values_mut()
                    .filter(|mapping| {
                        mapping.group_id_hex.as_deref() == event["group_id_hex"].as_str()
                    })
                    .flat_map(|mapping| mapping.prompt_queue.iter_mut())
                    .find(|prompt| {
                        Some(prompt.id.as_str()) == event["message"]["message_id_hex"].as_str()
                    })
                    && let Some(message) = &mut prompt.media
                {
                    message["_codr_prepared_text"] = event["_codr_prepared_text"].clone();
                }
                self.state.media_pending.remove(index);
                self.save()?;
                self.process_prompt_queues().await;
                return Ok(());
            }
            event["message"]["text"] = event["_codr_prepared_text"].clone();
            event["message"]["media"] = json!([]);
            let result = self.route_inbound(&event, true, false).await;
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
        if self.media_job.is_none() {
            let waiting: HashSet<_> = self
                .state
                .mappings
                .values()
                .filter_map(|mapping| {
                    mapping
                        .prompt_queue
                        .iter()
                        .find(|prompt| prompt.media.is_some())
                        .map(|prompt| (prompt.group.clone(), prompt.id.clone()))
                })
                .collect();
            let before = self.state.media_pending.len();
            self.state.media_pending.retain(|event| {
                event["_codr_queued_image"] != true
                    || waiting.contains(&(
                        event["group_id_hex"].as_str().unwrap_or("").to_owned(),
                        event["message"]["message_id_hex"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned(),
                    ))
            });
            if self.state.media_pending.len() != before {
                self.save()?;
            }
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

    fn announce_permission(
        &mut self,
        key: &str,
        group: &str,
        mut permission: output::permission::Permission,
    ) -> Result<()> {
        self.state.mappings.get_mut(key).unwrap().decision = None;
        if let Some(previous) = self.state.mappings[key].permission.as_ref()
            && previous.request == permission.request
        {
            permission = previous.clone();
        }
        if self.state.mappings[key]
            .permission_attempts
            .contains(&permission.token())
        {
            permission.submitted = true;
        }
        if !permission.announced && !permission.submitted {
            self.state.enqueue(group, &permission.text());
            permission.announced = true;
        }
        self.state.mappings.get_mut(key).unwrap().permission = Some(permission);
        self.save()
    }

    fn permission_notice(&mut self, key: &str, group: &str, id: &str, text: &str) -> Result<()> {
        let before = self.state.clone();
        self.state
            .mappings
            .get_mut(key)
            .unwrap()
            .seen_messages
            .push(id.into());
        self.state
            .enqueue_manual(group, text, &format!("permission:{group}:{id}"));
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        Ok(())
    }

    async fn reply_permission(
        &mut self,
        key: &str,
        group: &str,
        id: &str,
        text: &str,
        typed: bool,
    ) -> Result<()> {
        self.refresh_forwarding()?;
        let Some((reply, code)) = output::permission::parse_reply(text).filter(|_| typed) else {
            return self.permission_notice(
                key,
                group,
                id,
                "Use typed /allow CODE or /reject CODE. Persistent rules stay terminal-only.",
            );
        };
        let Some(permission) = self.state.mappings[key]
            .permission
            .clone()
            .filter(|permission| permission.announced)
        else {
            return self.permission_notice(
                key,
                group,
                id,
                "No permission card is available. Check the terminal.",
            );
        };
        if !permission.token().eq_ignore_ascii_case(code) {
            return self.permission_notice(
                key,
                group,
                id,
                "Permission code does not match. Use the current card.",
            );
        }
        if permission.submitted
            || self.state.mappings[key]
                .permission_attempts
                .contains(&permission.token())
        {
            return self.permission_notice(
                key,
                group,
                id,
                "A reply was already attempted. Check the terminal.",
            );
        }
        let Some(agent) = self.agents.get(key).cloned() else {
            return self.permission_notice(
                key,
                group,
                id,
                "Session is unavailable. No permission reply sent.",
            );
        };
        let Some(session) = agent
            .agent_session
            .as_ref()
            .filter(|session| session.agent == "opencode" && session.kind == "id")
        else {
            return self.permission_notice(key, group, id, "This permission needs the terminal.");
        };
        if herdr::get(&self.herdr, &agent).await?.agent_status != "blocked" {
            return self.permission_notice(key, group, id, "Permission is no longer waiting.");
        }
        let fresh = output::permission::read_request(
            &self.config.opencode_bin,
            &session.value,
            permission.id(),
        )
        .await;
        if !fresh.is_ok_and(|fresh| fresh.request == permission.request) {
            return self.permission_notice(
                key,
                group,
                id,
                "Permission changed or could not be verified. No reply sent.",
            );
        }
        if herdr::get(&self.herdr, &agent).await?.agent_status != "blocked" {
            return self.permission_notice(key, group, id, "Permission is no longer waiting.");
        }
        // Durable intent precedes the only mutation. Unknown results must never auto-retry.
        let before = self.state.clone();
        let mapping = self.state.mappings.get_mut(key).unwrap();
        mapping.permission_attempts.push(permission.token());
        mapping.permission.as_mut().unwrap().submitted = true;
        mapping.seen_messages.push(id.into());
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        let result = output::permission::reply(
            &self.config.opencode_bin,
            &session.value,
            permission.id(),
            reply,
        )
        .await;
        self.state.enqueue_manual(
            group,
            if result.is_ok() {
                "Permission reply sent."
            } else {
                "Permission reply not confirmed. Check the terminal; no automatic retry."
            },
            &format!("permission:{group}:{id}"),
        );
        self.save()
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
        custom_text: bool,
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
            if looks_like_answer {
                return self.decision_notice(key, group, id, "No supported decision is available yet. Wait for its question card or use the terminal. No answer was sent.");
            }
            return Ok(false);
        };
        if !looks_like_answer && !active.looks_like_answer(text) {
            return Ok(false);
        }
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
        let parsed = if custom_text {
            decision.answer(text)
        } else {
            decision.choice(text).map(output::decision::Answer::Choice)
        };
        let answer = match parsed {
            Ok(answer) => answer,
            Err(_) if !custom_text => return Ok(false),
            Err(error) => return self.decision_notice(key, group, id, &error.to_string()),
        };
        if matches!(&answer, output::decision::Answer::Choice(_)) && decision.form_id.is_none() {
            let visible = self.herdr.herdr("agent.read", json!({"target":agent.pane_id,"source":"visible","format":"text","lines":300,"strip_ansi":true})).await?;
            let matches = visible["result"]["read"]["truncated"] != true
                && visible["result"]["read"]["text"]
                    .as_str()
                    .is_some_and(|text| decision.matches_visible(text));
            if !matches || herdr::get(&self.herdr, agent).await?.agent_status != "blocked" {
                return self.decision_notice(key, group, id, "The terminal is no longer showing that decision's choice list. No answer was sent. Check the terminal or wait for its current question.");
            }
        } else if herdr::get(&self.herdr, agent).await?.agent_status != "blocked" {
            return self.decision_notice(
                key,
                group,
                id,
                "That question is no longer waiting. No answer was sent.",
            );
        }
        let pending_prompt = self.state.mappings[key].pending_prompt;
        let before = self.state.clone();
        let mapping = self.state.mappings.get_mut(key).unwrap();
        mapping.decision.as_mut().unwrap().submitted = true;
        mapping.pending_prompt = true;
        mapping.seen_messages.push(id.into());
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        let result = if let Some(form) = &decision.form_id {
            // V2 form option values equal their labels, as checked by from_forms.
            // Bind both choices and custom text to the verified request, not its UI.
            let value = match &answer {
                output::decision::Answer::Choice(choice) => {
                    &decision.questions[0].options[choice - 1].label
                }
                output::decision::Answer::Custom(text) => text,
            };
            let path = format!("/api/session/{}/form/{form}/reply", session.value);
            crate::catalogue::api(
                &self.config,
                "post",
                &path,
                Some(&json!({"answer":{"q0":value}})),
            )
            .await
            .map(|_| ())
        } else {
            let output::decision::Answer::Choice(choice) = &answer else {
                unreachable!("Custom answers require a native form");
            };
            herdr::choose(&self.herdr, agent, *choice).await
        };
        match result {
            Ok(()) => {
                let message = match answer {
                    output::decision::Answer::Choice(choice) => format!("Sent choice {choice}: {}.", decision.questions[0].options[choice-1].label),
                    output::decision::Answer::Custom(_) => "Sent your custom answer to the current question.".into(),
                };
                self.state.enqueue(group, &message);
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
            if mapping.control {
                continue;
            }
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
                agent.group_name_with_suffix(mapping.group_agent_suffix.as_deref())
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
                    "Workspace: unknown\nAgent: unknown\nManaged by codr via herdr.".into()
                });
            // MDK 0.13 exposes the subject but not the description. Remember only
            // confirmed metadata writes, so matching titles can still gain details.
            if info["subject"] == name
                && info["description"]
                    .as_str()
                    .or(mapping.group_description.as_deref())
                    == Some(description.as_str())
            {
                let m = self.state.mappings.get_mut(&key).unwrap();
                m.group_name = Some(name);
                m.group_description = Some(description);
                m.group_name_locked = false;
                m.rename_uncertain = false;
                self.save()?;
                continue;
            }
            // Profile updates have no idempotency key. Never retry an uncertain mutation.
            self.state.mappings.get_mut(&key).unwrap().rename_uncertain = true;
            self.save()?;
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
            let m = self.state.mappings.get_mut(&key).unwrap();
            m.group_name = Some(name.clone());
            m.group_description = Some(description);
            m.group_name_locked = false;
            m.rename_uncertain = false;
            self.save()?;
            println!("Renamed: {name}");
        }
        Ok(())
    }

    /// Keep a live group's title and description in step with herdr. A title the
    /// user changed is never overwritten, and an uncertain profile update is never retried.
    async fn sync_group_profile(&mut self, key: &str, agent: &Agent, group: &str) {
        if self.state.mappings[key].control {
            return;
        }
        let desired_name =
            agent.group_name_with_suffix(self.state.mappings[key].group_agent_suffix.as_deref());
        let desired_description = agent.group_description();
        let (last_name, last_description) = {
            let mapping = &self.state.mappings[key];
            if mapping.group_name_locked || mapping.rename_uncertain {
                return;
            }
            // Groups created before this field existed have no trusted title.
            // The manual rename-groups command seeds it and enables automatic renames.
            let Some(last_name) = mapping.group_name.clone() else {
                return;
            };
            (last_name, mapping.group_description.clone())
        };
        let name_changed = last_name != desired_name;
        let description_changed = last_description.as_deref() != Some(desired_description.as_str());
        if !name_changed && !description_changed {
            return;
        }
        let info = match self.group_info(group).await {
            Ok(info) => info,
            Err(error) => {
                if error.downcast_ref::<crate::wire::NotSubmitted>().is_none() {
                    eprintln_throttled!("Group profile check failed; keeping the current title.");
                }
                return;
            }
        };
        let subject = info["subject"].as_str().unwrap_or("");
        if subject != desired_name && subject != last_name {
            let mapping = self.state.mappings.get_mut(key).unwrap();
            mapping.group_name_locked = true;
            let _ = self.save();
            eprintln_throttled!("Group title was changed outside codr; leaving it unchanged.");
            return;
        }
        let current_description = info["description"].as_str().or(last_description.as_deref());
        if subject == desired_name && current_description == Some(desired_description.as_str()) {
            let mapping = self.state.mappings.get_mut(key).unwrap();
            mapping.group_name = Some(desired_name);
            mapping.group_description = Some(desired_description);
            let _ = self.save();
            return;
        }
        self.state.mappings.get_mut(key).unwrap().rename_uncertain = true;
        if self.save().is_err() {
            eprintln_throttled!("Group rename intent could not be saved; no update was submitted.");
            return;
        }
        let update = self
            .marmot
            .marmot(json!({
                "type": "group_profile_update",
                "account_id_hex": self.config.account_id_hex,
                "group_id_hex": group,
                "name": desired_name,
                "description": desired_description,
            }))
            .await;
        match update {
            Ok(result) => {
                let confirmed = result["type"] == "group_profile_updated"
                    && result["message_ids_hex"]
                        .as_array()
                        .is_some_and(|ids| !ids.is_empty());
                let verified = confirmed
                    && match self.group_info(group).await {
                        Ok(info) => {
                            info["subject"] == desired_name
                                && info["description"]
                                    .as_str()
                                    .is_none_or(|actual| actual == desired_description)
                        }
                        Err(_) => false,
                    };
                let mapping = self.state.mappings.get_mut(key).unwrap();
                if verified {
                    mapping.group_name = Some(desired_name.clone());
                    mapping.group_description = Some(desired_description);
                    mapping.rename_uncertain = false;
                    let _ = self.save();
                    eprintln_throttled!("Renamed group: {desired_name}");
                } else {
                    mapping.rename_uncertain = true;
                    let _ = self.save();
                    eprintln_throttled!(
                        "Group profile update was not confirmed; automatic renames stopped until rename-groups runs."
                    );
                }
            }
            Err(error) => {
                if error.downcast_ref::<crate::wire::NotSubmitted>().is_some() {
                    self.state.mappings.get_mut(key).unwrap().rename_uncertain = false;
                    if self.save().is_err() {
                        self.state.mappings.get_mut(key).unwrap().rename_uncertain = true;
                    }
                    return;
                }
                let mapping = self.state.mappings.get_mut(key).unwrap();
                mapping.rename_uncertain = true;
                let _ = self.save();
                eprintln_throttled!(
                    "Group profile update is uncertain; automatic renames stopped until rename-groups runs."
                );
            }
        }
    }

    async fn group_info(&self, group: &str) -> Result<Value> {
        let info = self
            .marmot
            .marmot(json!({"type":"group_info","account_id_hex":self.config.account_id_hex,"group_id_hex":group}))
            .await?;
        ensure!(
            info["type"] == "group_info",
            "Unexpected MDK group response"
        );
        Ok(info)
    }

    pub async fn run(&mut self, porter_enabled: bool) -> Result<()> {
        let porter_listener = if porter_enabled {
            Some(crate::porter::listen(&self.config.state_path)?)
        } else {
            None
        };
        if self.update_messaging_home.is_none() {
            crate::update::ready(self)?;
        }
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
        // Authoritative snapshots avoid relying on herdr's non-durable, differently shaped event payloads.
        // State-change sequence and stored assistant message IDs also catch work completed between snapshots.
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut recovery_due = true;
        let mut recovery_tick = tokio::time::interval(Duration::from_secs(60));
        recovery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                Ok(stream) = crate::porter::accept(&porter_listener) => {
                    if crate::porter::serve(self, stream).await.is_err() {
                        eprintln_throttled!("Porter request failed; no automatic mutation retry.");
                    }
                },
                _ = tokio::signal::ctrl_c() => {
                    subscriber.abort();
                    if let Some(job) = self.media_job.take() { job.abort(); }
                    return Ok(());
                },
                _ = tick.tick() => {
                    match herdr::snapshot(&self.herdr).await {
                        Ok(agents) => if self.reconcile(agents).await.is_err() { eprintln_throttled!("Session reconciliation failed; will retry."); },
                        Err(_) => { self.herdr_available = false; recovery_due = true; eprintln_throttled!("herdr is unavailable; waiting for reconnection."); }
                    }
                },
                _ = reads_ready.notified() => {
                    if self.herdr_available && self.poll_ready_sessions().await.is_err() {
                        eprintln_throttled!("Session reads remain pending.");
                    }
                },
                Some(event) = rx.recv() => {
                    if event["type"] == "reconcile_inbound" || event["type"] == "resync_required" {
                        // Refresh current agent identities before recovering any queued phone prompts.
                        recovery_due = true;
                    } else if self.inbound(&event).await.is_err() { recovery_due = true; eprintln_throttled!("Inbound message deferred; will reconcile history."); }
                },
                _ = recovery_tick.tick() => { recovery_due = true; }
            }
            if recovery_due && self.herdr_available {
                recovery_due = self.recover_inbound().await.is_err();
            }
            if self.update_messaging_home.is_some() && self.state.update_job.is_some() {
                // Failed messaging startup leaves desktop reads available, but
                // cannot acknowledge a healthy combined installation.
                let _ = crate::update::ready_when_healthy(self).await;
            }
            if crate::update::poll(self).await.is_err() {
                eprintln_throttled!(
                    "Update result remains pending; no automatic installation retry."
                );
            }
            if self.process_media().await.is_err() {
                recovery_due = true;
                eprintln_throttled!("Prepared media input deferred; will reconcile history.");
            }
            if self.flush().await.is_err() {
                eprintln_throttled!("MDK send unavailable; private outbox retained for retry.");
            }
            crate::porter::publish(self);
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
