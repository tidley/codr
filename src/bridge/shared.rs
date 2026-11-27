//! Shared groups select dedicated native mappings; transport and admission stay
//! in the normal bridge. No group member gains global or permission authority.
use super::*;

/// Native ownership is independent of prompt-delivery configuration and the
/// mutable outer agent label. Exclude `source` here so a changed source for the
/// same native conversation is detected before ordinary private discovery.
pub(super) fn native_identity(agent: &Agent) -> Option<(String, String, String)> {
    let session = agent.agent_session.as_ref()?;
    Some((
        session.agent.clone(),
        session.kind.clone(),
        session.value.clone(),
    ))
}

/// Retained ownership survives loss of optional Agent presentation metadata.
/// Check both sources: disagreeing saved evidence must not free either native
/// conversation for another room. Legacy terminal-only keys are not native IDs.
pub(super) fn owns_native_identity(
    key: &str,
    mapping: &Mapping,
    identity: &(String, String, String),
) -> bool {
    let from_key = serde_json::from_str::<[String; 4]>(key)
        .ok()
        .map(|[_, agent, kind, value]| (agent, kind, value));
    from_key.as_ref() == Some(identity)
        || mapping
            .agent_record
            .as_ref()
            .and_then(native_identity)
            .as_ref()
            == Some(identity)
}

fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 32
        && alias.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && alias
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Mentions must be whole whitespace-delimited tokens (optional comma/colon).
/// In particular, URLs, email addresses and partial aliases cannot select a peer.
fn addressed(text: &str, roster: &BTreeMap<String, String>) -> Result<(Vec<String>, String)> {
    let mut targets = Vec::new();
    let mut prompt = String::new();
    let mut copied = 0;
    let mut scanned = 0;
    for token in text.split_whitespace() {
        let start = scanned + text[scanned..].find(token).unwrap();
        scanned = start + token.len();
        if let Some(alias) = token.strip_prefix('@') {
            let alias = alias.trim_end_matches([',', ':']);
            let key = roster.get(alias).ok_or_else(|| anyhow::anyhow!("Unknown or incomplete agent alias. Use a complete configured @alias as a separate word; no agent was invoked."))?;
            if !targets.contains(key) {
                targets.push(key.clone());
            }
            prompt.push_str(&text[copied..start]);
            copied = scanned;
        }
    }
    prompt.push_str(&text[copied..]);
    Ok((targets, prompt))
}

impl Bridge {
    /// Same-user local operator only, while the worker lock is held. No roster
    /// mutation is exposed to chat senders. Existing mappings are never rebound.
    pub async fn attach_shared_chat(
        &mut self,
        group: &str,
        profiles: &[String],
        agents: &[(String, String)],
    ) -> Result<()> {
        ensure!(
            !profiles.is_empty()
                && !agents.is_empty()
                && agents.len() <= 16
                && profiles.len() <= 32,
            "Provide 1–32 explicit human npubs and 1–16 alias=native-key bindings"
        );
        let mut humans = Vec::new();
        for profile in profiles {
            let human = phone_hex(profile)?;
            ensure!(
                human != self.config.account_id_hex && !humans.contains(&human),
                "Duplicate human or bridge identity in allowlist"
            );
            humans.push(human);
        }
        let mut aliases = HashSet::new();
        let mut keys = HashSet::new();
        for (alias, key) in agents {
            ensure!(
                valid_alias(alias) && aliases.insert(alias) && keys.insert(key),
                "Invalid or duplicate shared alias/session"
            );
        }
        let mut prepared = Vec::new();
        let mut terminals = HashSet::new();
        let mut sessions = HashSet::new();
        for (alias, key) in agents {
            let mut mapping = self
                .prepare_chat_attachment(key, group, humans.clone())
                .await?;
            let agent = mapping.agent_record.as_ref().unwrap();
            ensure!(
                crate::prompt::native_session(&self.config, agent).is_some(),
                "Shared chats require native OpenCode ID sessions and a configured local service registration"
            );
            let native = native_identity(agent).unwrap();
            ensure!(
                sessions.insert(native),
                "Native OpenCode session already belongs to another binding"
            );
            ensure!(
                terminals.insert(agent.terminal_id.clone()),
                "Shared agents must use separate native terminals"
            );
            mapping.shared_alias = Some(alias.clone());
            prepared.push((key.clone(), mapping));
        }
        // All readers and group checks must succeed before any mapping exists.
        // Revalidate every native identity after the last potentially slow read.
        for (key, mapping) in &prepared {
            let expected = mapping.agent_record.as_ref().unwrap();
            let current = herdr::get(&self.herdr, expected).await?;
            ensure!(
                current.key() == *key
                    && current.terminal_id == expected.terminal_id
                    && current.pane_id == expected.pane_id
                    && current.state_change_seq == expected.state_change_seq
                    && current.settled(),
                "Shared native session changed during attachment"
            );
        }
        let before = self.state.clone();
        for (key, mut mapping) in prepared {
            mapping.attached_after = Some(now());
            self.state.mappings.insert(key, mapping);
        }
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn shared_inbound(&mut self, event: &Value) -> Result<bool> {
        let Some(group) = event["group_id_hex"].as_str() else {
            return Ok(false);
        };
        let peers: Vec<_> = self
            .state
            .mappings
            .iter()
            .filter(|(_, m)| m.group_id_hex.as_deref() == Some(group) && m.shared_alias.is_some())
            .map(|(key, _)| key.clone())
            .collect();
        if peers.is_empty() {
            return Ok(false);
        }
        if peers
            .iter()
            .all(|key| self.state.mappings[key].room.is_none())
        {
            // Alias-only multi-session sharing predates authenticated room
            // policy. Retain its identities/history, but never carry its owner
            // bypass or plaintext addressing into the accepted room contract.
            if event["account_id_hex"] != self.config.account_id_hex
                || event["type"] != "inbound_message"
            {
                return Ok(true);
            }
            let id = event["message"]["message_id_hex"].as_str().unwrap_or("");
            if id.is_empty() {
                return Ok(true);
            }
            let sender = event["message"]["sender"]["account_id_hex"]
                .as_str()
                .unwrap_or("");
            if sender != self.phone
                && !peers.iter().any(|key| {
                    self.state.mappings[key]
                        .authorized_profiles
                        .iter()
                        .any(|id| id == sender)
                })
            {
                return Ok(true);
            }
            if event["message"]["sender"]["is_self"] == true || sender == self.config.account_id_hex
            {
                return Ok(true);
            }
            if event["message"]["text"]
                .as_str()
                .unwrap_or("")
                .contains('@')
            {
                self.shared_refusal(&peers, group, id, "Alias-only sharing is disabled. Use a fresh dedicated strict room with authenticated access policy. Tool permissions remain terminal-only.")?;
            } else {
                self.shared_consume(&peers, id)?;
            }
            return Ok(true);
        }
        ensure!(
            event["marmot_agent_control"] == PROTOCOL,
            "MDK event protocol mismatch"
        );
        let msg = &event["message"];
        let sender = msg["sender"]["account_id_hex"].as_str().unwrap_or("");
        // Sender keys, not membership, roles or display labels, grant invocation.
        // Reject reflected bridge/bot/system envelopes before parsing their text.
        if event["type"] != "inbound_message"
            || event["account_id_hex"] != self.config.account_id_hex
            || sender == self.config.account_id_hex
            || msg["sender"]["is_self"] == true
            || ["is_bot", "is_agent", "is_system"]
                .iter()
                .any(|flag| msg["sender"][flag] == true)
            || msg["role"].as_str().is_some_and(|role| role != "user")
            || msg["sender"]["role"]
                .as_str()
                .is_some_and(|role| !matches!(role, "user" | "human"))
            || msg["kind"]
                .as_u64()
                .is_some_and(|kind| !matches!(kind, 9 | 1111))
        {
            return Ok(true);
        }
        let Some(id) = msg["message_id_hex"].as_str().filter(|id| !id.is_empty()) else {
            return Ok(true);
        };
        if !peers.iter().all(|key| {
            let m = &self.state.mappings[key];
            (sender == self.phone
                || m.authorized_profiles
                    .iter()
                    .any(|profile| profile == sender))
                && m.attached_after.is_some_and(|boundary| {
                    msg["recorded_at"]
                        .as_u64()
                        .is_some_and(|at| at > boundary && at <= now().saturating_add(5))
                })
        }) {
            return Ok(true);
        }
        let mut roster = BTreeMap::new();
        let mut invalid = false;
        for key in &peers {
            let alias = self.state.mappings[key].shared_alias.as_ref().unwrap();
            invalid |= !valid_alias(alias) || roster.insert(alias.clone(), key.clone()).is_some();
        }
        let parsed = if invalid {
            Err(anyhow::anyhow!(
                "Ambiguous shared roster. Ask the local owner to inspect it; no agent was invoked."
            ))
        } else {
            addressed(msg["text"].as_str().unwrap_or(""), &roster)
        };
        let (targets, text) = match parsed {
            Ok(result) => result,
            Err(error) => {
                self.shared_refusal(&peers, group, id, &error.to_string())?;
                return Ok(true);
            }
        };
        let media = ["attachments", "media"]
            .iter()
            .any(|field| msg[field].as_array().is_some_and(|a| !a.is_empty()));
        if media || msg["text_truncated"] == true || msg["attachments_truncated"] == true {
            self.shared_refusal(&peers, group, id, "Shared chat accepts complete text only. Send a new text message with an explicit @alias; attachments and voice cannot select an agent and were not submitted.")?;
            return Ok(true);
        }
        if text.trim_start().starts_with('/') || text.trim().eq_ignore_ascii_case("unmute") {
            self.shared_refusal(&peers, group, id, "Shared chat does not run bridge commands. The owner must use a private/control chat or the local operator commands. No prompt was sent.")?;
            return Ok(true);
        }
        if targets.is_empty() {
            // Ordinary conversation is consumed without waking global forwarding.
            self.shared_consume(&peers, id)?;
            return Ok(true);
        }
        let unselected: Vec<_> = peers
            .iter()
            .filter(|key| !targets.contains(key))
            .cloned()
            .collect();
        self.shared_consume(&unselected, id)?;
        let mut failed = false;
        for key in targets {
            if self.state.mappings[&key]
                .seen_messages
                .iter()
                .any(|seen| seen == id)
            {
                continue;
            }
            if !self
                .agents
                .get(&key)
                .is_some_and(|agent| crate::prompt::native_session(&self.config, agent).is_some())
            {
                crate::launch::reply(
                    self,
                    &key,
                    group,
                    id,
                    "Native OpenCode admission is unavailable. No prompt was sent; ask the owner to inspect the dedicated session and service registration.",
                )?;
                continue;
            }
            let mut child = event.clone();
            child["message"]["text"] = json!(text);
            // The explicit key is internal, never trusted from a transport field.
            // Each mapping persists consumption before its one native POST. A
            // failing peer must not stop other explicitly selected agents.
            if self
                .route_inbound_for(&child, false, false, Some(&key))
                .await
                .is_err()
            {
                failed = true
            }
        }
        ensure!(
            !failed,
            "Some addressed agents remain deferred; consumed admissions will not retry"
        );
        Ok(true)
    }

    fn shared_consume(&mut self, peers: &[String], id: &str) -> Result<()> {
        for key in peers {
            let seen = &mut self.state.mappings.get_mut(key).unwrap().seen_messages;
            if !seen.iter().any(|seen| seen == id) {
                seen.push(id.into())
            }
        }
        self.save()
    }

    fn shared_refusal(
        &mut self,
        peers: &[String],
        group: &str,
        id: &str,
        text: &str,
    ) -> Result<()> {
        if peers.iter().all(|key| {
            self.state.mappings[key]
                .seen_messages
                .iter()
                .any(|seen| seen == id)
        }) {
            return Ok(());
        }
        self.state.enqueue_manual(
            group,
            &format!("Codr: {text}"),
            &format!("shared:{group}:{id}"),
        );
        self.shared_consume(peers, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_exact_unique_and_preserve_prompt_formatting() {
        let roster = BTreeMap::from([
            ("builder".into(), "one".into()),
            ("reviewer".into(), "two".into()),
        ]);
        for text in [
            "@build fix",
            "@builder-extra fix",
            "@BUILDER fix",
            "@builder @unknown fix",
        ] {
            assert!(addressed(text, &roster).is_err(), "{text}");
        }
        assert!(
            addressed("mail@example.com https://x/@builder", &roster)
                .unwrap()
                .0
                .is_empty()
        );
        let (keys, text) =
            addressed("@builder @reviewer @builder\n```\n  code\n```", &roster).unwrap();
        assert_eq!(keys, ["one", "two"]);
        assert!(text.contains("\n```\n  code\n```"));
    }
}
