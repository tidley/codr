//! Owner-only phone grant controls. Aliases route; signed account identity rules.
use super::*;
use crate::rooms::{AccountRole, RoomPolicy};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn revision(
    policy: &RoomPolicy,
    key: &str,
    group: &str,
    alias: &str,
    account: &str,
    owner: &str,
) -> String {
    // Include the native key as well as the transport: a new dedicated binding
    // must not accept a menu copied from an old conversation with the same alias.
    let bytes = serde_json::to_vec(&(key, group, alias, account, owner, policy.access_revision()))
        .expect("Room control identity tuple serializes");
    hex::encode(&Sha256::digest(bytes)[..12])
}

fn commands(alias: &str, token: &str) -> String {
    format!(
        "Revision: {token}\n/agents @{alias} allow MEMBER_ACCOUNT {token}\n/agents @{alias} revoke MEMBER_ACCOUNT {token}\nInvocation only; no tool approvals. Accepted native work is unchanged."
    )
}

impl Bridge {
    pub(super) async fn room_phone_control(
        &mut self,
        key: &str,
        group: &str,
        id: &str,
        context: &Value,
    ) -> Result<bool> {
        let message = &context["message"];
        let words: Vec<_> = message["text"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        if words.first().copied() != Some("/agents") {
            return Ok(false);
        }
        let alias = self.state.mappings[key].shared_alias.clone().unwrap();
        let addresses: Vec<String> = serde_json::from_value(message["addressed_accounts"].clone())?;
        let addressed = addresses.contains(&self.config.account_id_hex);
        let explicit = words.get(1).filter(|word| word.starts_with('@')).copied();
        let targeted = match explicit {
            Some(target) => target == format!("@{alias}") && (addresses.is_empty() || addressed),
            None => addressed,
        };
        let members = Self::room_members(context)?;
        let at = message["recorded_at"].as_u64().unwrap_or(0);
        let owner_request = targeted
            && message["sender_account_id_hex"] == self.phone
            && members.contains(&self.phone)
            && members.contains(&self.config.account_id_hex)
            && message["unsupported_media"] == false
            && self.state.mappings[key]
                .attached_after
                .is_some_and(|boundary| at > boundary && at <= now().saturating_add(5));
        // Even rejected/wrong-target controls are consumed without invocation.
        // Do not ask the native backend, private dispatcher or model to parse them.
        let verified = if owner_request {
            let dispatch = self.room_context(group, Some(id)).await?;
            Self::room_members(&dispatch)? == members && dispatch["message"] == context["message"]
        } else {
            false
        };
        // Context reads may have persisted a roster hold. Capture rollback state
        // afterwards so a grant/reply save error cannot erase that observed hold.
        let before = self.state.clone();
        if !self
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .room
            .as_mut()
            .unwrap()
            .consume_control(id)
        {
            return Ok(true);
        }
        let reply = if !owner_request {
            None
        } else if !verified {
            Some("Room context changed. No grant changed. Reconcile current membership with local room.roster, then request /agents again.".to_owned())
        } else {
            let args = &words[if explicit.is_some() { 2 } else { 1 }..];
            Some(self.room_access_reply(key, group, &alias, args, &members))
        };
        if let Some(reply) = reply {
            self.state.enqueue_agent_manual(
                key,
                group,
                &reply,
                &format!("room-control:{group}:{id}"),
            );
        }
        // Dedup, grant/CAS revision and acknowledgement intent are one durable
        // state update. No network send happens before this succeeds.
        if let Err(error) = self.save() {
            self.state = before;
            return Err(error);
        }
        Ok(true)
    }

    fn room_access_reply(
        &mut self,
        key: &str,
        group: &str,
        alias: &str,
        args: &[&str],
        members: &BTreeSet<String>,
    ) -> String {
        let policy = self
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .room
            .as_mut()
            .unwrap();
        let Ok(access) = policy.invocation_access(&self.phone, members) else {
            return format!(
                "Access is paused until the owner reconciles all current member roles with local room.roster. No roles or grants were guessed. Then send /agents @{alias} again."
            );
        };
        let token = revision(
            policy,
            key,
            group,
            alias,
            &self.config.account_id_hex,
            &self.phone,
        );
        if args.is_empty() {
            let mut text = format!(
                "Invocation access for @{alias}\nTarget account: {}\n",
                self.config.account_id_hex
            );
            for (id, role, allowed) in access {
                let role = match role {
                    AccountRole::Human => "human",
                    AccountRole::Agent => "agent",
                };
                text.push_str(&format!(
                    "{role} {id}: {}\n",
                    if allowed { "allowed" } else { "denied" }
                ));
            }
            text.push_str(&commands(alias, &token));
            return text;
        }
        let [action, identity, supplied] = args else {
            return commands(alias, &token);
        };
        if !matches!(*action, "allow" | "revoke") {
            return commands(alias, &token);
        }
        if *supplied != token {
            return format!(
                "This control is stale or belongs to another target. No grant changed. Send /agents @{alias} again."
            );
        }
        let account = if crate::rooms::valid_id(identity) {
            Some((*identity).to_owned())
        } else {
            phone_hex(identity).ok()
        };
        let Some((account, role, _)) = access
            .into_iter()
            .find(|(id, _, _)| Some(id) == account.as_ref())
        else {
            return format!(
                "Choose a currently classified peer account (full hex or npub) from /agents @{alias}. No grant changed."
            );
        };
        let allowed = *action == "allow";
        if policy
            .set_allowed(&self.phone, &account, role, allowed)
            .is_err()
        {
            return "Access revision is unavailable. No grant changed; inspect local owner controls.".into();
        }
        let token = revision(
            policy,
            key,
            group,
            alias,
            &self.config.account_id_hex,
            &self.phone,
        );
        format!(
            "{} invocation for {account}.\n{}",
            if allowed { "Allowed" } else { "Revoked" },
            commands(alias, &token)
        )
    }
}
