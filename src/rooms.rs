//! Per-local-target invocation grants, not tool or application permissions.
//! Identities and addressing are supplied by the authenticated MDK host, never
//! inferred from aliases, display names, leaf counts or client role claims.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountRole {
    Human,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPolicy {
    owner: String,
    agent: String,
    roster: BTreeMap<String, AccountRole>,
    /// Missing on pre-latch state means unconfirmed, never implicitly trusted.
    /// Only an explicit owner roster reconciliation may clear an observed drift.
    #[serde(default)]
    roster_validated: bool,
    /// Monotonic access-control CAS revision, independent of invocation/budget.
    /// Default zero permits existing state to acquire its first phone menu.
    #[serde(default)]
    access_revision: u64,
    humans_allowed: BTreeSet<String>,
    agents_allowed: BTreeSet<String>,
    stopped: bool,
    agent_enabled: bool,
    agent_remaining: u8,
    seen: BTreeSet<String>,
}

pub fn valid_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl RoomPolicy {
    pub fn new(owner: String, agent: String) -> Result<Self> {
        ensure!(
            valid_id(&owner) && valid_id(&agent) && owner != agent,
            "Distinct canonical owner and agent account IDs required"
        );
        Ok(Self {
            owner,
            agent,
            roster: BTreeMap::new(),
            roster_validated: false,
            access_revision: 0,
            humans_allowed: BTreeSet::new(),
            agents_allowed: BTreeSet::new(),
            stopped: false,
            agent_enabled: false,
            agent_remaining: 0,
            seen: BTreeSet::new(),
        })
    }
    fn require_owner(&self, actor: &str) -> Result<()> {
        ensure!(
            actor == self.owner,
            "Only this agent's owner can change room policy"
        );
        Ok(())
    }
    pub fn agent(&self) -> &str {
        &self.agent
    }
    pub fn accounts(&self) -> Vec<String> {
        self.roster.keys().cloned().collect()
    }
    pub fn seen(&self, id: &str) -> bool {
        self.seen.contains(id)
    }
    /// Consume controls without treating the owner as an invoking human or
    /// renewing a finite agent cycle. The bridge persists this with reply intent.
    pub(crate) fn consume_control(&mut self, message: &str) -> bool {
        valid_id(message) && self.seen.insert(message.into())
    }
    pub(crate) fn access_revision(&self) -> u64 {
        self.access_revision
    }
    pub(crate) fn invocation_access(
        &self,
        actor: &str,
        members: &BTreeSet<String>,
    ) -> Result<Vec<(String, AccountRole, bool)>> {
        self.require_owner(actor)?;
        ensure!(
            self.roster_validated
                && self.roster.keys().cloned().collect::<BTreeSet<_>>() == *members,
            "Owner must reconcile the authenticated roster"
        );
        Ok(self
            .roster
            .iter()
            .filter(|(id, _)| **id != self.agent)
            .map(|(id, role)| {
                let allowed = match role {
                    AccountRole::Human => self.humans_allowed.contains(id),
                    AccountRole::Agent => self.agents_allowed.contains(id),
                };
                (id.clone(), *role, allowed)
            })
            .collect())
    }
    fn advance_access_revision(&mut self) -> Result<()> {
        self.access_revision = self
            .access_revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Access revision exhausted"))?;
        Ok(())
    }
    /// Call for every authenticated roster observation, including dispatch
    /// rechecks. Returning to an old roster must not silently restore authority.
    pub fn observe_members(&mut self, members: &BTreeSet<String>) -> bool {
        if self.roster.keys().cloned().collect::<BTreeSet<_>>() == *members {
            return false;
        }
        let before = self.clone();
        self.roster_validated = false;
        self.agent_remaining = 0;
        self.humans_allowed.retain(|id| members.contains(id));
        self.agents_allowed.retain(|id| members.contains(id));
        let changed = *self != before;
        if changed {
            // Saturation never wraps an old menu back into validity. Explicit
            // mutations then fail closed through checked revision advancement.
            self.access_revision = self.access_revision.saturating_add(1);
        }
        changed
    }
    pub fn set_roster(&mut self, actor: &str, roster: BTreeMap<String, AccountRole>) -> Result<()> {
        self.require_owner(actor)?;
        ensure!(
            roster.keys().all(|id| valid_id(id))
                && roster.get(&self.owner) == Some(&AccountRole::Human)
                && roster.get(&self.agent) == Some(&AccountRole::Agent),
            "Classify the complete authenticated roster, including owner and local agent"
        );
        self.advance_access_revision()?;
        if self.roster != roster {
            self.humans_allowed
                .retain(|id| roster.get(id) == Some(&AccountRole::Human));
            self.agents_allowed
                .retain(|id| roster.get(id) == Some(&AccountRole::Agent));
            self.agent_remaining = 0;
            self.roster = roster;
        }
        // Reconciliation can remove a hold, but is not a new human invocation.
        self.agent_remaining = 0;
        self.roster_validated = true;
        Ok(())
    }
    pub fn set_allowed(
        &mut self,
        actor: &str,
        account: &str,
        role: AccountRole,
        allowed: bool,
    ) -> Result<()> {
        self.require_owner(actor)?;
        ensure!(
            self.roster.get(account) == Some(&role) && account != self.agent,
            "Grant must name a classified peer account"
        );
        // Also advance on a no-op grant: an accepted owner choice is single-use,
        // including host changes and ABA grant/revoke round trips.
        self.advance_access_revision()?;
        let list = match role {
            AccountRole::Human => &mut self.humans_allowed,
            AccountRole::Agent => &mut self.agents_allowed,
        };
        if allowed {
            list.insert(account.into());
        } else {
            list.remove(account);
        }
        Ok(())
    }
    pub fn set_agent_enabled(&mut self, actor: &str, enabled: bool) -> Result<()> {
        self.require_owner(actor)?;
        self.agent_enabled = enabled;
        if !enabled {
            self.agent_remaining = 0;
        }
        // Enabling/resuming cannot manufacture a new four-message cycle.
        Ok(())
    }
    pub fn set_stopped(&mut self, actor: &str, stopped: bool) -> Result<()> {
        self.require_owner(actor)?;
        self.stopped = stopped;
        if stopped {
            self.agent_remaining = 0;
        }
        Ok(())
    }
    pub fn authorizes(&self, sender: &str, addressed: bool, members: &BTreeSet<String>) -> bool {
        if self.stopped
            || !self.roster_validated
            || self.agent_remaining > 4
            || self.roster.is_empty()
            || self.roster.keys().cloned().collect::<BTreeSet<_>>() != *members
            || self
                .roster
                .values()
                .filter(|role| **role == AccountRole::Human)
                .count()
                >= 2
                && !addressed
        {
            return false;
        }
        match self.roster.get(sender) {
            // Owner control is deliberately NOT an implicit invocation grant.
            Some(AccountRole::Human) => self.humans_allowed.contains(sender),
            Some(AccountRole::Agent) => {
                sender != self.agent
                    && self.agent_enabled
                    && self.agent_remaining > 0
                    && self.agents_allowed.contains(sender)
            }
            None => false,
        }
    }
    /// Persist this mutation before native admission. Refusals are consumed too:
    /// later grants or membership repair must never authorize old room traffic.
    pub fn accept(
        &mut self,
        sender: &str,
        message: &str,
        addressed: bool,
        members: &BTreeSet<String>,
    ) -> bool {
        self.observe_members(members);
        if !valid_id(message)
            || !self.seen.insert(message.into())
            || !self.authorizes(sender, addressed, members)
        {
            return false;
        }
        match self.roster.get(sender) {
            Some(AccountRole::Human) => {
                self.agent_remaining = if self.agent_enabled { 4 } else { 0 }
            }
            Some(AccountRole::Agent) => self.agent_remaining -= 1,
            None => return false,
        }
        true
    }
}
