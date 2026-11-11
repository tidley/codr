# Persistence I/O optimization plan

**Goal:** Remove redundant bridge-state writes without delaying durable delivery,
permission, account, or setup changes. Keep version 0.11.1 and existing data.

**Design:** Keep JSON state and atomic, file-and-directory-synced replacement.
The single worker owns a bounded in-memory cache of the last successfully saved
bytes and file metadata. Identical saves skip I/O beyond a metadata check. A
changed path, changed file, or failed write invalidates this cache. Polling clocks
are runtime data: read old fields for compatibility but omit them on new saves;
restart may refresh a transcript or decision sooner. Keep forwarding controls
fresh at existing safety boundaries. Do not change MDK storage or OpenCode data.

## Execution

- [x] Add bridge integration regressions for unchanged saves, immediate durable
  mutations, restart, polling-only changes, file replacement, and write retry.
  Use a held file descriptor to distinguish atomic replacements without inode
  reuse or sleeps. Run `cargo test --locked --test bridge state_save` and confirm
  the redundant-write assertions fail before implementation.
- [x] In `src/state.rs`, add a worker-local writer cache. Serialize once per save,
  compare exact bytes and Unix file identity/change metadata, invalidate before
  writing, and update the cache only after both syncs succeed. Keep uncached
  `BridgeState::save` and control-file writes compatible. Omit only
  `last_export_at` and `decision_checked_at` from serialization.
- [x] In `src/bridge.rs`, use this writer at open and save. Continue publishing
  desktop state even when the disk write is skipped. Add no debounce or delay.
- [x] Run the regression tests, then bridge, state, decision, permission, media,
  setup, update, and reader tests plus strict Clippy and formatting. Confirm
  build outputs use the central fastbuild adapter. Measure repeated unchanged
  saves on synthetic data, not the live account.
- [x] Document behavior and audit limits in `docs/REFERENCE.md` and
  `CHANGELOG.md`. Obtain two independent read-only reviews, fix demonstrated
  findings, and repeat relevant verification. Leave deployment and publication
  separate.

## Review scope and limits

Inspected JSON loading/saving and locks, forwarding controls, native transcript
readers, incremental OpenCode readers, desktop projections, media files, update
receipts, and embedded messaging ownership. Codex rereads its transcript on each
snapshot; Claude already caches appended records; OpenCode readers have event
checkpoints. Those are external-history reads, not the demonstrated internal
state-write flood. MDK database tuning needs its own measured evidence; do not
weaken durability, remove records, migrate data, or modify vendored dependencies
speculatively.

## Verification results

- The unchanged-save and polling-clock regressions failed before implementation
  and passed after it. The new tests also cover immediate persistence, restart,
  legacy clocks, missing/modified files, changed destination, failed save retry,
  rollback, and private state-file modes.
- A syscall trace of the synthetic 36-mapping test showed five replacements and
  ten syncs for startup, fixture persistence, delivery mutation and restart.
  The 100 intervening unchanged saves added no replacement or sync calls.
  This is a synthetic result, not a measured live-service throughput reduction.
- Rust verification: 482 tests passed across the full-suite attempt and a
  subsequent run of the remaining persistence-dependent binaries; one local-data
  test was ignored. Two embedded-runtime tests failed at socket startup in the
  full-suite attempt. The direct-runtime test reproduced by exact name with
  `socket_path_too_long`; the managed temporary path exceeds the private socket
  staging limit. No adapter, socket validation, or unrelated test was changed.
  The full suite is therefore not green.
- All 174 Python tests passed. Strict all-target Clippy, formatting and diff
  whitespace checks passed. Cargo reported outputs under the managed fastbuild
  target directory.
- Two independent read-only reviews found no actionable defects. Coverage does
  not inject directory-sync failure after rename or simulate power loss. Such a
  failure invalidates the cache by construction, but was not fault-injected.
- No deployment, account operation, release, publication, version change, or
  commit was performed. Existing dirty source changes were preserved.
