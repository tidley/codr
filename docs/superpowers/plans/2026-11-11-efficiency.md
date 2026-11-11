# Sequential efficiency implementation plan

**Goal:** Address the six reviewed Codr hot paths without changing the release,
account storage, phone authorization or delivery durability.

**Design:** Retain synchronous durable state boundaries. Cache forwarding reads
by file identity/change metadata, not time. Compare a persisted state snapshot
before JSON encoding, excluding only the two runtime clocks. Target reader
completion application without changing authoritative discovery. Use shared
native readers for manual history collection and bounded background jobs whose
results are applied serially. Recover full retained history in bounded batches
with backoff, never a sender-time watermark. Share immutable history snapshots.
No database migration, receipt eviction, or live deployment is included.

## Sequence

1. `src/forwarding.rs`, `src/bridge.rs`: test repeated-read cache reuse, atomic
   mute replacement, same-file changes, deletion, malformed data and retry. Keep
   locked mutations uncached and reload on every metadata change/error.
2. `src/state.rs`, `src/porter.rs`: compare the last successful typed state before
   encoding; ignore runtime clocks only. Skip desktop projection without
   subscribers. Test no-op cache reuse and durable mutation/failure behavior.
3. `src/reads.rs`, `src/bridge.rs`: track completed keys and apply those only.
   Keep full reconciliation for forwarding activation and ended-session work.
   Test no-ready wake is a no-op, fast/slow readers, stale state, and exits.
4. `src/reads.rs`, `src/bridge.rs`: share the persistent reader and concurrency
   budget. Collect manual history outside dispatch with bounded pending jobs;
   apply results only against the same account/group/session binding, and retain
   durable command intent across restart. Preserve catch-up ordering and spacing.
5. `src/bridge.rs`: collect recovery pages away from dispatch, apply serially in
   bounded batches, and back off failures. Preserve full-history late-delivery
   correctness. Test slow recovery, retry cadence, and late older rows.
6. `src/output/service.rs`, `src/output.rs`, `src/bridge.rs`: share immutable
   history storage, separate mutable controls, and retain current reader APIs
   where practical. Test shared unchanged history and changed-history isolation.

For each item: add a focused regression, confirm failure, implement, then run
the relevant test/module. Before completion run formatting, strict all-target
Clippy, affected Rust suites and Python tests. Obtain two independent read-only
reviews of this task's diff, fix demonstrated findings and verify again. Record
the existing embedded-runtime temporary-socket path limitation separately.

## Implementation record

All six steps were implemented in the listed order. Focused regressions were
observed failing before their implementation and passing afterwards. The manual
history queue also has restart and failed-save retry coverage.

Independent review identified two defects in the first combined candidate:
fresh phone validation joined the background FIFO, and the recovery channel
still followed an unbounded payload collection. Both were corrected. Foreground
validation retains its prior independent path; a saturation regression proves
it reaches its reader even when all four background permits are occupied.
Recovery now seeks the oldest page afresh, then streams forward through a
bounded channel only when unseen owner input exists. Its forward cursor is
local to the scan, never a persistent ingestion watermark. Backpressure and
late-older-row regressions pass. Quiet scans avoid the second traversal; a
backlog needs the extra forward traversal to keep payload memory bounded.

The correctness recheck also found that canonical group ordering can cross
epochs with decreasing timestamps. Recovery now detects cycles by cursor
identity, not timestamp ordering. The multi-page decreasing-clock regression
failed before that correction and passed afterwards. Cursor identities are
retained for cycle checks; payload storage remains page/channel bounded.

No deployment, release, version change, receipt pruning or database migration is
part of this change. The installed persistence-only candidate stays running.

## Final verification

- 492 Rust tests passed; eight local-data tests remained ignored. The two
  previously demonstrated managed-temporary-socket-path failures were explicitly
  excluded, not counted as passing.
- All 174 Python tests passed. Strict all-target Clippy, formatting and diff
  whitespace checks passed on the corrected source.
- Both independent read-only reviewers approved the final corrections with no
  remaining demonstrated findings.
- Source snapshots, hashes and verification records are private under
  `~/.local/state/codr/feature-verification/20261111T141410Z-sequential-efficiency/`.
  These records include the new source files, which remain untracked alongside
  the existing uncommitted work. No live performance gain is claimed until a
  separately authorized deployment is measured.
