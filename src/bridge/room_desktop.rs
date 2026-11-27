//! Explicit same-user desktop admission, not an export of native terminal input.
//! The room post is signed by Codr. Its owner label grants no room authority.
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, sync::Arc};

impl Bridge {
    pub(super) async fn room_desktop_send(&mut self, key: &str, request: &Value) -> Result<Value> {
        let request_id =
            uuid::Uuid::parse_str(request["request_id"].as_str().unwrap_or(""))?.to_string();
        let text = request["text"].as_str().unwrap_or("");
        // A native prompt hook prepares the ORIGINAL immutable message ID. It
        // must never call send_reserved and create a second user turn.
        let prepared_id = if request["method"] == "room.desktop_prepare" {
            let id = request["native_message_id"].as_str().unwrap_or("");
            ensure!(
                id.starts_with("msg_")
                    && id.len() <= 128
                    && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "Valid native message ID required"
            );
            Some(id)
        } else {
            None
        };
        ensure!(
            !text.trim().is_empty() && text.len() <= 30000 && !text.trim_start().starts_with('/'),
            "Desktop room instructions require complete non-command text"
        );
        let mut identity = serde_json::to_vec(&(
            &self.config.account_id_hex,
            key,
            &request["terminal_id"],
            text,
        ))?;
        if let Some(id) = prepared_id {
            identity.extend_from_slice(id.as_bytes());
        }
        let fingerprint = hex::encode(Sha256::digest(identity));
        if let Some(operation) = self.state.desktop_operations.get(&request_id) {
            ensure!(
                operation.fingerprint == fingerprint,
                "Desktop request ID was reused"
            );
            let result = operation.result.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "Desktop relay is uncertain; inspect its receipt, never automatically resend"
                )
            })?;
            if let Some(id) = prepared_id {
                // Hook preparation is not admission. A cancelled native call
                // can retry this ID later, so recheck current policy WITHOUT
                // publishing again or renewing the finite interaction budget.
                let retained = self.state.mappings[key]
                    .agent_record
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Room native provenance unavailable"))?;
                let _reservation = self.room_reservation(key)?;
                self.desktop_room_ready(key, &retained, Some(id)).await?;
            }
            return Ok(result);
        }
        let retained = self.state.mappings[key]
            .agent_record
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Room native provenance unavailable"))?;
        ensure!(
            request["terminal_id"] == retained.terminal_id,
            "Desktop target changed"
        );
        let reservation = match self.room_reservation(key) {
            Ok(guard) => guard,
            Err(error) => {
                self.quarantine_room(key, "native_room_ownership_unverified")?;
                return Err(error);
            }
        };
        if let Some(id) = prepared_id {
            ensure!(
                !self.state.mappings[key]
                    .room_input_fingerprints
                    .contains_key(id),
                "Native message already reserved by another desktop operation"
            );
        }
        self.desktop_room_ready(key, &retained, prepared_id).await?;
        let group = self.state.mappings[key].group_id_hex.clone().unwrap();
        // Durable intent precedes BOTH external operations. Neither a lost room
        // receipt nor a lost native receipt permits a second send on retry.
        let before = self.state.clone();
        self.state.desktop_operations.insert(
            request_id.clone(),
            crate::state::DesktopOperation {
                fingerprint,
                result: None,
            },
        );
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        let mirror = self
            .marmot
            .marmot(json!({"type":"send_final",
                "account_id_hex":self.config.account_id_hex,"group_id_hex":group,
                "text":format!("Desktop via Codr\n\n{text}"),
                "reply_to_message_id_hex":null,"idempotency_key":format!("desktop:{request_id}")
            }))
            .await?;
        ensure!(
            mirror["type"] == "final_sent",
            "Desktop room publication unconfirmed"
        );
        let ids: Vec<String> = serde_json::from_value(mirror["message_ids_hex"].clone())?;
        ensure!(
            !ids.is_empty() && ids.iter().all(|id| crate::rooms::valid_id(id)),
            "Desktop room publication has no valid receipt"
        );
        // Native input or membership may have changed while publication waited.
        // The visible mirror is NOT authority to bypass those later checks.
        let (history, members) = self.desktop_room_ready(key, &retained, prepared_id).await?;
        let native_session = crate::prompt::native_session(&self.config, &retained)
            .ok_or_else(|| anyhow::anyhow!("Native V2 admission unavailable"))?
            .to_owned();
        let id = &ids[0];
        let native_id = prepared_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("msg_codr_{}", uuid::Uuid::new_v4().simple()));
        ensure!(
            !self.state.mappings[key]
                .room_input_fingerprints
                .contains_key(&native_id),
            "Native message already reserved by another desktop operation"
        );
        let before = self.state.clone();
        let mapping = self.state.mappings.get_mut(key).unwrap();
        // The private same-UID host endpoint authenticates the local owner. This
        // is not an inbound human event: no sender/recipient fields are invented.
        // Explicit target selection still requires the owner's invocation grant.
        ensure!(
            mapping
                .room
                .as_mut()
                .unwrap()
                .accept(&self.phone, id, true, &members),
            "Desktop instruction no longer authorised"
        );
        let mut submission =
            crate::activity::Submission::new(id, text, Some(&history.user_inputs), now());
        submission.native_message_id = Some(native_id.clone());
        mapping
            .shared_admissions
            .insert(id.clone(), native_id.clone());
        mapping.room_input_fingerprints.insert(
            native_id.clone(),
            crate::activity::UserInput::new(String::new(), text)
                .fingerprint
                .unwrap(),
        );
        mapping.seen_messages.extend(ids.iter().cloned());
        mapping.submissions.push(submission);
        mapping.pending_prompt = true;
        mapping.completion_pending = true;
        mapping.activity.start_phone(id, text, &history.user_inputs);
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        let admission = if prepared_id.is_some() {
            // The hook returns only after this intent is durable. OpenCode then
            // admits this exact ID/text through its normal prompt path.
            "prepared"
        } else {
            match crate::prompt::send_reserved(&self.config, &native_session, &native_id, text)
                .await
            {
                Ok(input) => {
                    self.observe_submission_acceptances(key, &[input]);
                    self.state
                        .mappings
                        .get_mut(key)
                        .unwrap()
                        .activity
                        .confirm_phone(id);
                    "accepted"
                }
                Err(_) => "uncertain",
            }
        };
        let result = json!({"request_id":request_id,"published":true,
            "message_ids_hex":ids,"native_message_id":native_id,"native_admission":admission});
        self.state
            .desktop_operations
            .get_mut(&request_id)
            .unwrap()
            .result = Some(result.clone());
        self.save()?;
        drop(reservation);
        Ok(result)
    }

    async fn desktop_room_ready(
        &mut self,
        key: &str,
        retained: &Agent,
        prepared_id: Option<&str>,
    ) -> Result<(Arc<output::SessionHistory>, BTreeSet<String>)> {
        let mapping = &self.state.mappings[key];
        ensure!(
            mapping.room_quarantined.is_none()
                && mapping
                    .submissions
                    .iter()
                    .all(|s| prepared_id.is_some() && s.native_message_id.as_deref() == prepared_id)
                && mapping.prompt_queue.is_empty() && mapping.queue_flight.is_none(),
            "Room is held or has unfinished input"
        );
        let history = self
            .reads
            .fresh(&self.config, retained.agent_session.as_ref().unwrap())
            .await?
            .history;
        self.observe_activity(key, &history)?;
        let response = self
            .herdr
            .herdr("agent.get", json!({"target":retained.pane_id}))
            .await?;
        let live: Agent = serde_json::from_value(response["result"]["agent"].clone())?;
        if live.key() != key
            || live.terminal_id != retained.terminal_id
            || live.pane_id != retained.pane_id
        {
            self.quarantine_room(key, "native_identity_changed")?;
            anyhow::bail!("Room native identity changed");
        }
        // The native UI may show working while its prompt hook is still waiting
        // for admission. Prepared input is queued by the hook; it is not a second
        // native POST and must not depend on optimistic terminal idle detection.
        ensure!(
            (prepared_id.is_some() || live.settled()) && live.agent_status != "blocked",
            "Room agent is not ready"
        );
        ensure!(
            crate::prompt::native_session(&self.config, &live).is_some(),
            "Native V2 admission unavailable"
        );
        let group = self.state.mappings[key].group_id_hex.clone().unwrap();
        let context = self.room_context(&group, None).await?;
        let members = Self::room_members(&context)?;
        ensure!(
            self.state.mappings[key]
                .room
                .as_ref()
                .unwrap()
                .authorizes(&self.phone, true, &members),
            "Owner invocation grant, resumed room and reconciled roster required"
        );
        Ok((history, members))
    }
}
