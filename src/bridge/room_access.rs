//! Strict room seam: authenticated MDK events never enter private commands.
use super::*;
use crate::rooms::{AccountRole, RoomPolicy};
use std::collections::{BTreeMap, BTreeSet};

impl Bridge {
    pub(crate) async fn human_control(&mut self, request: &Value) -> Result<Value> {
        use sha2::{Digest, Sha256};
        ensure!(
            request["bridge_account"] == self.config.account_id_hex,
            "Human host account changed"
        );
        let account = request["human_account"].as_str().unwrap_or("");
        ensure!(
            crate::rooms::valid_id(account) && account != self.config.account_id_hex,
            "Choose a distinct external human account"
        );
        let id = uuid::Uuid::parse_str(request["request_id"].as_str().unwrap_or(""))?.to_string();
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(request)?));
        if let Some(operation) = self.state.human_operations.get(&id) {
            ensure!(
                operation.fingerprint == fingerprint,
                "Human request identity was reused"
            );
            return operation.result.clone().ok_or_else(|| {
                anyhow::anyhow!("Human operation is uncertain; no automatic replay")
            });
        }
        let command = match request["method"].as_str() {
            Some("human.register") => {
                json!({"type":"human_account_register","account_id_hex":account,"signer_socket":request["signer_socket"]})
            }
            Some("human.login") => {
                json!({"type":"human_account_login","account_id_hex":account,"signer_socket":request["signer_socket"]})
            }
            Some("human.send") => {
                json!({"type":"human_account_send","account_id_hex":account,"group_id_hex":request["group"],
                "text":request["text"],"addressed_accounts":request["addressed_accounts"]})
            }
            _ => anyhow::bail!("Unknown human account operation"),
        };
        let before = self.state.clone();
        self.state.human_operations.insert(
            id.clone(),
            crate::state::HumanOperation {
                fingerprint,
                result: None,
            },
        );
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        // No synthetic human envelope or agent-account send is created here.
        // Refused/unavailable signers remain failed/uncertain, never a key fallback.
        let response = self.marmot.marmot(command).await?;
        ensure!(
            matches!(response["type"].as_str(), Some("ack" | "final_sent")),
            "External human operation not confirmed"
        );
        self.state.human_operations.get_mut(&id).unwrap().result = Some(response.clone());
        self.save()?;
        Ok(response)
    }
    pub(super) async fn room_context(
        &mut self,
        group: &str,
        message: Option<&str>,
    ) -> Result<Value> {
        let context = self
            .marmot
            .marmot(
                json!({"type":"room_context", "account_id_hex":self.config.account_id_hex,
            "group_id_hex":group,"message_id_hex":message}),
            )
            .await?;
        ensure!(
            context["type"] == "room_context"
                && context["account_id_hex"] == self.config.account_id_hex
                && context["group_id_hex"] == group,
            "Authenticated room context unavailable"
        );
        let members = Self::room_members(&context)?;
        let mut changed = false;
        for mapping in self
            .state
            .mappings
            .values_mut()
            .filter(|m| m.group_id_hex.as_deref() == Some(group))
        {
            if let Some(policy) = mapping.room.as_mut() {
                changed |= policy.observe_members(&members);
            }
        }
        // This hold precedes later message/native validation. Even a failed or
        // duplicate dispatch cannot undo an authenticated roster observation.
        if changed {
            self.save()?;
        }
        Ok(context)
    }
    pub(super) fn room_members(context: &Value) -> Result<BTreeSet<String>> {
        let members: Vec<String> = serde_json::from_value(context["members"].clone())?;
        ensure!(
            !members.is_empty()
                && members.len() <= 256
                && members.iter().all(|id| crate::rooms::valid_id(id)),
            "Invalid authenticated account roster"
        );
        let unique: BTreeSet<_> = members.iter().cloned().collect();
        ensure!(
            unique.len() == members.len(),
            "Duplicate account roster identities"
        );
        Ok(unique)
    }
    /// One dedicated transport configuration owns one fresh room session. No
    /// existing private state/history is converted into a shared transport.
    pub async fn attach_room(&mut self, key: &str, group: &str, alias: &str) -> Result<()> {
        self.attach_room_with_setup(key, group, alias, None).await
    }

    pub(crate) async fn attach_reserved_room(
        &mut self,
        key: &str,
        group: &str,
        alias: &str,
        setup: &crate::native_rooms::Guard,
    ) -> Result<()> {
        self.attach_room_with_setup(key, group, alias, Some(setup))
            .await
    }

    async fn attach_room_with_setup(
        &mut self,
        key: &str,
        group: &str,
        alias: &str,
        setup: Option<&crate::native_rooms::Guard>,
    ) -> Result<()> {
        ensure!(
            self.state.mappings.is_empty()
                && !alias.is_empty()
                && alias.len() <= 32
                && alias
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "Use an unused dedicated transport state and stable lowercase alias"
        );
        let context = self.room_context(group, None).await?;
        let members = Self::room_members(&context)?;
        ensure!(
            members.contains(&self.phone) && members.contains(&self.config.account_id_hex),
            "Owner and agent must be authenticated room members"
        );
        let mut mapping = self
            .prepare_chat_attachment_with_setup(key, group, vec![], setup)
            .await?;
        let agent = mapping.agent_record.as_ref().unwrap();
        ensure!(
            crate::prompt::native_session(&self.config, agent).is_some(),
            "Room requires native OpenCode V2 admission"
        );
        // Serialize the empty-history check with every native POST, including
        // private transports. The persistent claim itself has no lease/expiry.
        let claim = crate::native_rooms::Claim::new(&self.config, agent, group)?;
        let owned = if setup.is_none() {
            Some(crate::native_rooms::guard(
                &self.config,
                crate::native_rooms::identity(agent)?,
            )?)
        } else {
            None
        };
        let reservation = setup.or(owned.as_ref()).unwrap();
        let history = self
            .reads
            .fresh(&self.config, agent.agent_session.as_ref().unwrap())
            .await?
            .history;
        ensure!(
            history.user_inputs.is_empty()
                && history.finals.is_empty()
                && history.turns.is_empty()
                && history.recent_messages.is_empty(),
            "Room requires a fresh empty native conversation"
        );
        if setup.is_some() {
            reservation.finish_setup(&self.config, &claim)?;
        } else {
            reservation.reserve(&claim)?;
        }
        mapping.shared_alias = Some(alias.into());
        mapping.room = Some(RoomPolicy::new(
            self.phone.clone(),
            self.config.account_id_hex.clone(),
        )?);
        self.state.mappings.insert(key.into(), mapping);
        if let Err(error) = self.save() {
            self.state.mappings.remove(key);
            return Err(error);
        }
        Ok(())
    }
    pub(crate) async fn room_control(&mut self, request: &Value) -> Result<Value> {
        ensure!(
            request["bridge_account"] == self.config.account_id_hex,
            "Room transport account changed"
        );
        let key = request["session_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Native key required"))?;
        let operation = request["method"].as_str().unwrap_or("");
        if operation == "room.attach" {
            self.attach_room(
                key,
                request["group"].as_str().unwrap_or(""),
                request["alias"].as_str().unwrap_or(""),
            )
            .await?;
            return Ok(json!({"attached":true,"invocation_grants":"empty"}));
        }
        let mapping = self
            .state
            .mappings
            .get(key)
            .filter(|m| m.room.is_some())
            .ok_or_else(|| anyhow::anyhow!("Unknown strict room"))?;
        if operation == "room.status" {
            return Ok(json!({"policy":mapping.room,"quarantined":mapping.room_quarantined}));
        }
        if matches!(operation, "room.desktop_send" | "room.desktop_prepare") {
            return self.room_desktop_send(key, request).await;
        }
        let group = mapping.group_id_hex.clone().unwrap();
        let before;
        if operation == "room.roster" {
            let roles: BTreeMap<String, AccountRole> =
                serde_json::from_value(request["roles"].clone())?;
            let context = self.room_context(&group, None).await?;
            ensure!(
                roles.keys().cloned().collect::<BTreeSet<_>>() == Self::room_members(&context)?,
                "Classify every authenticated account"
            );
            before = self.state.clone();
            self.state
                .mappings
                .get_mut(key)
                .unwrap()
                .room
                .as_mut()
                .unwrap()
                .set_roster(&self.phone, roles)?;
        } else {
            before = self.state.clone();
            let policy = self
                .state
                .mappings
                .get_mut(key)
                .unwrap()
                .room
                .as_mut()
                .unwrap();
            match operation {
                "room.grant" => policy.set_allowed(
                    &self.phone,
                    request["account"].as_str().unwrap_or(""),
                    serde_json::from_value(request["role"].clone())?,
                    request["allowed"]
                        .as_bool()
                        .ok_or_else(|| anyhow::anyhow!("Explicit grant decision required"))?,
                )?,
                "room.agent_budget" => policy.set_agent_enabled(
                    &self.phone,
                    request["enabled"]
                        .as_bool()
                        .ok_or_else(|| anyhow::anyhow!("Explicit budget decision required"))?,
                )?,
                "room.stop" => policy.set_stopped(&self.phone, true)?,
                "room.resume" => policy.set_stopped(&self.phone, false)?,
                _ => anyhow::bail!("Unknown room owner operation"),
            }
        }
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        Ok(json!({"saved":true}))
    }
    pub(super) fn quarantine_room(&mut self, key: &str, reason: &str) -> Result<()> {
        let mapping = self.state.mappings.get_mut(key).unwrap();
        mapping.room_quarantined = Some(reason.into());
        let group = mapping.group_id_hex.clone();
        // Private native input may already be in a queued reply. Persist the
        // quarantine and remove that output before any later flush can disclose it.
        self.state
            .outbox
            .retain(|item| Some(&item.group) != group.as_ref());
        self.save()
    }
    pub(super) async fn room_inbound(&mut self, event: &Value) -> Result<bool> {
        let group = event["group_id_hex"].as_str().unwrap_or("");
        let Some(key) = self
            .state
            .mappings
            .iter()
            .find(|(_, m)| m.room.is_some() && m.group_id_hex.as_deref() == Some(group))
            .map(|(key, _)| key.clone())
        else {
            return Ok(false);
        };
        ensure!(
            event["marmot_agent_control"] == PROTOCOL,
            "Room event protocol mismatch"
        );
        if event["type"] != "inbound_message"
            || event["account_id_hex"] != self.config.account_id_hex
        {
            return Ok(true);
        }
        let id = event["message"]["message_id_hex"].as_str().unwrap_or("");
        // Attachment captures existing IDs independently of their timestamps.
        // A slightly future transport clock must not replay baseline traffic.
        if self.state.mappings[&key]
            .seen_messages
            .iter()
            .any(|seen| seen == id)
            || !crate::rooms::valid_id(id)
            || self.state.mappings[&key].room.as_ref().unwrap().seen(id)
        {
            return Ok(true);
        }
        let context = self.room_context(group, Some(id)).await?;
        let message = &context["message"];
        ensure!(
            message["message_id_hex"] == id,
            "Authenticated room message unavailable"
        );
        // Explicit owner access controls are authenticated room operations, not
        // native prompts. They must remain usable without an invocation grant,
        // idle backend, enabled budget or resumed native target.
        if self.room_phone_control(&key, group, id, &context).await? {
            return Ok(true);
        }
        if self.state.mappings[&key].room_quarantined.is_some() {
            return Ok(true);
        }
        let sender = message["sender_account_id_hex"].as_str().unwrap_or("");
        let text = message["text"].as_str().unwrap_or("");
        let addressed: Vec<String> = serde_json::from_value(message["addressed_accounts"].clone())?;
        let members = Self::room_members(&context)?;
        let current = self
            .agents
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Room native identity unavailable"))?;
        let retained = self.state.mappings[&key].agent_record.clone().unwrap();
        let reservation = match self.room_reservation(&key) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.quarantine_room(&key, "native_room_ownership_unverified")?;
                return Err(error);
            }
        };
        if current.key() != retained.key()
            || current.terminal_id != retained.terminal_id
            || current.pane_id != retained.pane_id
        {
            self.quarantine_room(&key, "native_identity_changed")?;
            return Ok(true);
        }
        let history = self
            .reads
            .fresh(&self.config, current.agent_session.as_ref().unwrap())
            .await?
            .history;
        self.observe_activity(&key, &history)?;
        // Read the raw authoritative record here: the private get helper turns
        // identity drift into a generic error before room quarantine can persist.
        let response = self
            .herdr
            .herdr("agent.get", json!({"target":current.pane_id}))
            .await?;
        let live: Agent = serde_json::from_value(response["result"]["agent"].clone())?;
        if live.key() != key
            || live.terminal_id != retained.terminal_id
            || live.pane_id != retained.pane_id
        {
            // A replacement observed after the snapshot is a durable safety
            // boundary, not a transient read error that permits queued replies.
            self.quarantine_room(&key, "native_identity_changed")?;
            return Ok(true);
        }
        // Recheck at dispatch after native validation: revoked grants, a changed
        // roster or changed event context cannot ride an earlier receipt check.
        let dispatch = self.room_context(group, Some(id)).await?;
        let dispatch_members = Self::room_members(&dispatch)?;
        if dispatch_members != members || dispatch["message"] != context["message"] {
            self.state
                .mappings
                .get_mut(&key)
                .unwrap()
                .room
                .as_mut()
                .unwrap()
                .accept("", id, false, &dispatch_members);
            self.save()?;
            return Ok(true);
        }
        let native_session = crate::prompt::native_session(&self.config, &current)
            .ok_or_else(|| anyhow::anyhow!("Native V2 room admission unavailable"))?
            .to_owned();
        let before = self.state.clone();
        let mapping = self.state.mappings.get_mut(&key).unwrap();
        let at = message["recorded_at"].as_u64().unwrap_or(0);
        let usable = mapping
            .attached_after
            .is_some_and(|boundary| at > boundary && at <= now().saturating_add(5))
            && message["unsupported_media"] == false
            && !text.trim().is_empty()
            && !text.trim_start().starts_with('/')
            && live.agent_status != "blocked"
            && mapping.submissions.is_empty();
        // Refused controls/media are consumed without private commands or tool
        // approvals, and without renewing a finite agent interaction cycle.
        if !usable {
            mapping
                .room
                .as_mut()
                .unwrap()
                .accept("", id, false, &members);
            if let Err(error) = self.save() {
                self.state = before;
                return Err(error);
            }
            return Ok(true);
        }
        if !mapping.room.as_mut().unwrap().accept(
            sender,
            id,
            addressed.iter().any(|id| id == &self.config.account_id_hex),
            &members,
        ) {
            if let Err(error) = self.save() {
                self.state = before;
                return Err(error);
            }
            return Ok(true);
        }
        let native_id = format!("msg_codr_{}", uuid::Uuid::new_v4().simple());
        let mut submission =
            crate::activity::Submission::new(id, text, Some(&history.user_inputs), now());
        submission.native_message_id = Some(native_id.clone());
        mapping
            .shared_admissions
            .insert(id.into(), native_id.clone());
        mapping.room_input_fingerprints.insert(
            native_id.clone(),
            crate::activity::UserInput::new(String::new(), text)
                .fingerprint
                .unwrap(),
        );
        mapping.seen_messages.push(id.into());
        mapping.submissions.push(submission);
        mapping.pending_prompt = true;
        mapping.completion_pending = true;
        mapping.activity.start_phone(id, text, &history.user_inputs);
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        // Intent, dedup and finite budget precede the only POST. Missing native
        // receipts are reconciled read-only; no retry or terminal fallback exists.
        // reservation remains locked through acceptance/uncertainty persistence.
        match crate::prompt::send_reserved(&self.config, &native_session, &native_id, text).await {
            Ok(input) => { self.observe_submission_acceptances(&key, &[input]); self.state.mappings.get_mut(&key).unwrap().activity.confirm_phone(id); },
            Err(_) => self.state.enqueue_agent(&key, group, "Native room admission is uncertain. No automatic retry or terminal fallback will run."),
        }
        self.save()?;
        drop(reservation);
        Ok(true)
    }

    pub(super) fn room_reservation(&self, key: &str) -> Result<crate::native_rooms::Guard> {
        let mapping = &self.state.mappings[key];
        let agent = mapping
            .agent_record
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Room native provenance unavailable"))?;
        let group = mapping
            .group_id_hex
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Room group unavailable"))?;
        let claim = crate::native_rooms::Claim::new(&self.config, agent, group)?;
        let guard =
            crate::native_rooms::guard(&self.config, crate::native_rooms::identity(agent)?)?;
        guard.verify(&claim)?;
        Ok(guard)
    }

    pub(super) fn native_room_conflict(&self, key: &str) -> Result<bool> {
        let mapping = &self.state.mappings[key];
        if let Some(agent) = &mapping.agent_record {
            return crate::native_rooms::binding_conflict(
                &self.config,
                agent,
                mapping.group_id_hex.as_deref(),
                mapping.room.is_some(),
            );
        }
        let native = mapping
            .agent_record
            .as_ref()
            .and_then(|a| crate::native_rooms::identity(a).ok())
            .or_else(|| {
                serde_json::from_str::<[String; 4]>(key)
                    .ok()
                    .map(|[_, a, k, v]| [a, k, v])
            });
        native.map_or(Ok(false), |identity| {
            crate::native_rooms::claimed(&self.config, &identity)
        })
    }
}
