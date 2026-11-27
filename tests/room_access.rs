use herdr_white_noise_bridge::rooms::{AccountRole, RoomPolicy};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn accepted_room_policy_never_grants_owner_implicit_invocation() {
    // The integrated policy must survive state round trips, start with empty
    // directional grants, and distinguish owner control from model invocation.
    let policy = RoomPolicy::new("11".repeat(32), "aa".repeat(32)).unwrap();
    assert!(!policy.authorizes(
        &"11".repeat(32),
        true,
        &std::collections::BTreeSet::from(["11".repeat(32), "aa".repeat(32)])
    ));
}

#[test]
fn room_grants_are_directional_and_agent_count_does_not_require_a_mention() {
    let owner = "11".repeat(32);
    let first = "aa".repeat(32);
    let second = "bb".repeat(32);
    let roles = BTreeMap::from([
        (owner.clone(), AccountRole::Human),
        (first.clone(), AccountRole::Agent),
        (second.clone(), AccountRole::Agent),
    ]);
    let members: BTreeSet<_> = roles.keys().cloned().collect();
    let mut a = RoomPolicy::new(owner.clone(), first.clone()).unwrap();
    let mut b = RoomPolicy::new(owner.clone(), second.clone()).unwrap();
    a.set_roster(&owner, roles.clone()).unwrap();
    b.set_roster(&owner, roles).unwrap();
    a.set_allowed(&owner, &owner, AccountRole::Human, true)
        .unwrap();
    a.set_allowed(&owner, &second, AccountRole::Agent, true)
        .unwrap();
    assert!(
        a.authorizes(&owner, false, &members),
        "Multiple agents alone do not require addressing"
    );
    assert!(
        !b.authorizes(&owner, true, &members),
        "Grants on A cannot grant invocation of B"
    );
    assert!(
        a.set_allowed(&second, &owner, AccountRole::Human, true)
            .is_err()
    );
    assert!(
        !a.authorizes(&second, true, &members),
        "An agent grant without owner-enabled finite budget grants no invocation"
    );
    a.set_agent_enabled(&owner, true).unwrap();
    assert!(
        !a.authorizes(&second, true, &members),
        "Enabling cannot manufacture an accepted-human boundary"
    );
    assert!(a.accept(&owner, &format!("{:064x}", 1), false, &members));
    for id in 2..6 {
        assert!(a.accept(&second, &format!("{id:064x}"), false, &members));
    }
    assert!(!a.accept(&owner, &format!("{:064x}", 1), false, &members));
    assert!(!a.accept(&second, &format!("{:064x}", 6), false, &members));
    let bytes = serde_json::to_vec(&a).unwrap();
    let restored: RoomPolicy = serde_json::from_slice(&bytes).unwrap();
    assert!(!restored.authorizes(&second, true, &members));
}

// Start with a real accepted human boundary so roster invalidation must revoke
// usable authority, rather than merely preserve an already-empty budget.
fn room_with_live_budget() -> (RoomPolicy, BTreeMap<String, AccountRole>) {
    let owner = "11".repeat(32);
    let agent = "aa".repeat(32);
    let peer = "bb".repeat(32);
    let roles = BTreeMap::from([
        (owner.clone(), AccountRole::Human),
        (agent.clone(), AccountRole::Agent),
        (peer.clone(), AccountRole::Agent),
    ]);
    let members = roles.keys().cloned().collect();
    let mut policy = RoomPolicy::new(owner.clone(), agent).unwrap();
    policy.set_roster(&owner, roles.clone()).unwrap();
    policy
        .set_allowed(&owner, &owner, AccountRole::Human, true)
        .unwrap();
    policy
        .set_allowed(&owner, &peer, AccountRole::Agent, true)
        .unwrap();
    policy.set_agent_enabled(&owner, true).unwrap();
    assert!(policy.accept(&owner, &format!("{:064x}", 1), true, &members));
    assert!(policy.authorizes(&peer, true, &members));
    (policy, roles)
}

#[test]
fn roster_hold_survives_return_restart_and_unrelated_owner_controls() {
    let (mut policy, roles) = room_with_live_budget();
    let owner = "11".repeat(32);
    let peer = "bb".repeat(32);
    let members: BTreeSet<_> = roles.keys().cloned().collect();
    let mut changed_members = members.clone();
    changed_members.insert("22".repeat(32));
    assert!(policy.observe_members(&changed_members));
    assert!(!policy.observe_members(&changed_members));
    assert!(!policy.observe_members(&members));

    let mut restored: RoomPolicy =
        serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
    restored.set_stopped(&owner, true).unwrap();
    restored.set_stopped(&owner, false).unwrap();
    restored.set_agent_enabled(&owner, false).unwrap();
    restored.set_agent_enabled(&owner, true).unwrap();
    restored
        .set_allowed(&owner, &owner, AccountRole::Human, true)
        .unwrap();
    restored
        .set_allowed(&owner, &peer, AccountRole::Agent, true)
        .unwrap();
    assert!(!restored.authorizes(&owner, true, &members));
    assert!(!restored.authorizes(&peer, true, &members));
    assert!(!restored.accept(&owner, &format!("{:064x}", 2), true, &members));
    assert!(restored.set_roster(&peer, roles.clone()).is_err());
    assert!(!restored.authorizes(&owner, true, &members));

    restored.set_roster(&owner, roles).unwrap();
    assert!(restored.authorizes(&owner, true, &members));
    assert!(!restored.authorizes(&peer, true, &members));
    // Neither the original accepted event nor an event refused during the hold
    // may renew the cycle after explicit reconciliation.
    for id in [1, 2] {
        assert!(!restored.accept(&owner, &format!("{id:064x}"), true, &members));
        assert!(!restored.authorizes(&peer, true, &members));
    }
    assert!(restored.accept(&owner, &format!("{:064x}", 3), true, &members));
    for id in 4..8 {
        assert!(restored.accept(&peer, &format!("{id:064x}"), true, &members));
    }
    assert!(!restored.accept(&peer, &format!("{:064x}", 8), true, &members));
}

#[test]
fn duplicate_receipt_still_latches_authenticated_roster_drift() {
    let (mut policy, roles) = room_with_live_budget();
    let owner = "11".repeat(32);
    let members: BTreeSet<_> = roles.keys().cloned().collect();
    let mut changed_members = members.clone();
    changed_members.insert("22".repeat(32));
    // The duplicate was already accepted; it cannot bypass roster observation.
    assert!(!policy.accept(&owner, &format!("{:064x}", 1), true, &changed_members));
    let restored: RoomPolicy =
        serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
    assert!(!restored.authorizes(&owner, true, &members));
    assert!(!restored.authorizes(&"bb".repeat(32), true, &members));
}

#[test]
fn removed_account_grant_does_not_return_with_reconciled_roster() {
    let (mut policy, roles) = room_with_live_budget();
    let owner = "11".repeat(32);
    let peer = "bb".repeat(32);
    let members: BTreeSet<_> = roles.keys().cloned().collect();
    let mut reduced = members.clone();
    reduced.remove(&peer);
    assert!(policy.observe_members(&reduced));
    let mut restored: RoomPolicy =
        serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
    restored.set_roster(&owner, roles).unwrap();
    assert!(restored.accept(&owner, &format!("{:064x}", 2), true, &members));
    assert!(!restored.authorizes(&peer, true, &members));
    restored
        .set_allowed(&owner, &peer, AccountRole::Agent, true)
        .unwrap();
    assert!(restored.authorizes(&peer, true, &members));
}

#[test]
fn legacy_policy_requires_reconciliation_without_renewing_duplicate_human_budget() {
    let (policy, roles) = room_with_live_budget();
    let owner = "11".repeat(32);
    let peer = "bb".repeat(32);
    let members = roles.keys().cloned().collect();
    let mut legacy = serde_json::to_value(&policy).unwrap();
    legacy.as_object_mut().unwrap().remove("roster_validated");
    let mut restored: RoomPolicy = serde_json::from_value(legacy).unwrap();
    assert!(!restored.authorizes(&owner, true, &members));
    assert!(!restored.authorizes(&peer, true, &members));
    restored.set_roster(&owner, roles).unwrap();
    assert!(restored.authorizes(&owner, true, &members));
    assert!(!restored.authorizes(&peer, true, &members));
    assert!(!restored.accept(&owner, &format!("{:064x}", 1), true, &members));
    assert!(!restored.authorizes(&peer, true, &members));
    assert!(restored.accept(&owner, &format!("{:064x}", 2), true, &members));
    assert!(restored.authorizes(&peer, true, &members));
}

#[test]
fn roster_reconciliation_does_not_resume_a_persisted_stop() {
    let (mut policy, roles) = room_with_live_budget();
    let owner = "11".repeat(32);
    let members = roles.keys().cloned().collect();
    policy.set_stopped(&owner, true).unwrap();
    let mut restored: RoomPolicy =
        serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
    restored.set_roster(&owner, roles).unwrap();
    assert!(!restored.authorizes(&owner, true, &members));
    restored.set_stopped(&owner, false).unwrap();
    assert!(restored.authorizes(&owner, true, &members));
    assert!(!restored.authorizes(&"bb".repeat(32), true, &members));
}
