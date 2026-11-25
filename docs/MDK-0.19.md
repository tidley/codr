# MDK 0.19 source upgrade and local handoff

Tracks [tidley/codr#31](https://github.com/tidley/codr/issues/31), separately from
release packaging #24 and unfinished progress feature #26. The clean source base
is `12fd101cdcf353e3251c19b7a88ecdca089edb55`. Codr remains version 0.12.2;
this working source is not a new release or a locally installed runtime.

## Source contract

Stable `v0.19.0` has annotated tag object
`3a8ed1270d72822e93dee04c1b09b29b902a0cf1` and peels to
`3fce95465cdea1edc8456dbd1c6691d1c8dc1f2e`. Read-only upstream resolution
confirmed `wn-agent-v0.19.0`, `marmotkit-v0.19.0` and `marmotc-v0.19.0` peel to
that same commit. No nightly or unreleased marmot-agents source is used.

`connector.toml` pins that commit. `vendor/mdk` carries its agent-control,
agent-connector and marmot-app crates plus their required schema/catalogue files
and protocol-test golden fixture.
Other MDK crates use that exact Git revision. `patches/mdk-codr.patch` describes
the local crate changes against the unmodified release for standalone connector
builds. The OpenMLS family pin, hax patch and SQLCipher 4.17 backport are retained;
the root lockfile includes `url` 2.5.9 (RUSTSEC-2026-0592).

MDK removed live-preview stream APIs and their QUIC crates in 0.19. Codr already
uses durable `send_final` and message edits; it does not reintroduce removed
requests. Parent-linked kind-1111 thread messages now count as chat upstream.
Timeline recovery retains owner, availability and seen-ID checks; media lookup
remains opt-in and excludes deleted/invalidated/non-chat rows. Native prompt
acceptance/reconciliation, uncertain-send no-retry rules, session mappings,
receipt history and owner-only controls remain Codr's existing contracts.

## Storage is a separately gated operation

Read upstream's [0.18 integration guide](https://github.com/marmot-protocol/mdk/blob/v0.19.0/docs/integration/0.18.0.md)
and [0.19 integration guide](https://github.com/marmot-protocol/mdk/blob/v0.19.0/docs/integration/0.19.0.md)
at the pinned release. This source declares `mdk-0.19`, deliberately incompatible
with automatic updates from `mdk-0.17` and `mdk-0.14`.

| Database/projection | 0.17 | 0.19 |
| --- | --- | --- |
| Account schema | 207 | 218 |
| Shared schema | 4 | 5 |
| Directory cache | 2 | 3 |
| Chat-list projection | Earlier projection | Version 5, rebuilt once |

0.18 runs account migrations 0208–0216; 0.19 runs 0217–0218. The comment/thread
repairs retain source events, media references and author edits. Migration 0218
removes stream tables, the stream timeline column and kind-1200 timeline rows;
retained source events and durable kind-9 finals survive. First open can take
time on large accounts. Device groups are enabled by default; usage-diagnostics
consent can require renewed acceptance. No consent is granted by this upgrade.

Before any live open, the coordinator must inspect the **actual installed**
cohort and every reader of its home (including background services/tools), then
establish the exact migration path. Older cohorts also require the 0.15–0.17
guides. Do not infer the installed schema from this repository's pin.

After a specific owner migration decision, and only in the agreed stopped
window, all readers must be stopped and a private device-local cold backup
verified. Include the home, database sidecars, encrypted media, bridge config,
routing/receipt state, service definitions, previous executable and required
secret-store credentials. Exclude `accounts/*/handoff/` plaintext attachment
leases per upstream. Keep credentials out of analysis, logs and chat. This worker
does not inspect or copy credentials, open live databases, stop services or install.

Do not run old and new readers together. Old binaries refuse upgraded schemas;
binary-only downgrade and hand-editing migration ledgers are unsupported. A
pre-upgrade copy is only a possible rollback before newer MLS traffic, under a
separate decision; never automatically restore stale MLS state after traffic.
The existing 0.14 handover helper is not a 0.19 migration tool.

## Qualification and later local validation

Supervisor-owned fixed-snapshot gates are `cargo test --locked --tests`,
`cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`,
and `python3 -m unittest discover -s tests -p 'test_*.py' -v`, from the root,
plus two independent read-only exact-candidate reviews. Results must be recorded
by the supervisor; their existence here is not a pass claim. Central fastbuild
adapters and the pinned Odin installation are required. Rust 1.97.1 and Odin
`dev-2026-10` (`84bc3fc`) remain unchanged. No release build/CI is triggered here.

Also qualify the patched standalone connector's agent-control/agent-connector
tests, including opt-in media/default/deletion regressions, and verify that its
patch reproduces the vendored crate source. Root integration tests exercise the
embedded lifecycle without creating accounts. A fixture result does not prove a
populated historical account migration or phone interoperability.

Before an authorised local installation, record the exact reviewed Codr source
snapshot, build inputs, binary SHA-256 and build/test evidence. No such installed
artifact is claimed here. After the separately approved handover, bounded checks
must verify the existing account identity, original chat/session mapping,
history/receipts, one owner prompt's native acceptance, durable reply, media and
clean shutdown/reopen. Do not create a diagnostic live agent, invite a phone,
replay old history or reset accounts as a smoke test. Report source qualification,
artifact qualification, installed runtime validation and publication separately.

## Worker evidence and unresolved qualification

The reconciled candidate keeps the same clean source base. Prior workflow
evidence records an unsuccessful Rust check (exit 78) and no independent reviews;
it does not qualify this candidate. Its detailed external log was inaccessible
to this worker. The earlier worker's disk-exhaustion report is historical, not a
current failed-check diagnosis.

Fresh reconciliation verification:

- Read-only release inspection confirms v0.19.0 is published, stable and not a
  draft (`2026-11-22T13:20:02Z`); all four cohort tags resolve as recorded above.
  Both integration guides and the 0.19 release contract were read at that commit.
- All four `test_connector_source.py`, ten `test_combined_setup.py` and 22
  `test_handover.py` fixture tests pass, including refusal of older cohorts before
  service mutation. The prior report of pre-upgrade failing source regressions
  was not re-executed on the base during reconciliation.
- All three crate inventories match stable upstream. The 398 unchanged crate,
  schema and catalogue files match its Git blobs. The six local patch files
  reverse to the exact stable blobs and reapply byte-for-byte. Patch SHA-256:
  `4343845f133f58c4851487f50377b6311cbff39427941ca556024629816755d3`.
- `cargo fmt --manifest-path vendor/mdk/Cargo.toml --all --check` and
  `git diff HEAD --check` pass. Neither is the supervisor's root formatting gate.
- `cargo test --locked --lib thread_recovery_keeps_owner_visibility_and_receipt_guards`
  and `cargo test --locked --test media_recovery` each pass one test. The first
  compile timed out after two minutes; a five-minute bounded continuation finished
  successfully. Outputs use the central adapter's
  `/mnt/fastbuild/cargo/worker-f192206be70e82df` directory, not a filesystem fallback.
- `cargo test --locked -p agent-control` first failed its existing rich-context
  golden test because the fixture was absent. Adding the unmodified stable
  `fixtures/agent-control-v2-rich-context.json` repairs it; all 28 tests then pass.
  The copied fixture matches upstream byte-for-byte (SHA-256
  `208f543a973aaa23a0f8b691b5c72aa0706d63d9ee09b2cd1426fcc2ecd580ed`).
- A focused `agent-connector` unit-test attempt from the root was refused by
  Cargo before compilation because its dev-dependencies require workspace
  membership. Run those tests in the vendor workspace or patched full upstream
  workspace; this refusal is not a passing connector test.

The supervisor owns the four fresh fixed-snapshot checks and two independent
reviews; none is claimed here. At the last capacity inspection the mounted
fastbuild volume had 7.3 GiB available, so the earlier zero-space condition was
not reproduced. Monitor capacity through the approved storage mechanism; no
cleanup or cache remapping was performed. Standalone connector tests,
populated-account migration fixtures, packaged artifacts and live phone/runtime
validation remain unclaimed. No commit, push, release, installation, service
activation or live storage open was performed by this worker.
