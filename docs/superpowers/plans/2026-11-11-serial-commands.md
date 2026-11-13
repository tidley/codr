# Serial phone controls

**Goal:** Accept `/recent /unmute` and other short sequences of phone controls.

Support two to eight explicitly slash-prefixed control commands, separated by
whitespace or newlines. Keep existing command prefixes and compact history counts.
Allow only help, status, queue, last, recent, sync and unmute. Validate every
command and argument before any dispatch. Keep launches, updates, permission
answers, ordinary prompts and paths outside this grammar.

Run existing handlers left-to-right after the normal account, owner, group,
complete-text and replay checks. Each command gets a stable internal receipt ID
derived from the original event and its index. Mark the original event consumed
only after every handler accepted its command. Stop on an error. A replay retries
unfinished work without repeating completed handlers. Background history intents
remain durable and do not wait for collection or delivery before the next command.
This is ordered dispatch, not an atomic transaction or ordered network delivery.

1. Add a regression for the screenshot, sequential sync effects, restart replay,
   partial errors and invalid/foreign/media input. Observe failure first.
2. Add a bounded control-sequence parser to `src/commands.rs`. Reuse normalisation
   and validate arguments without dispatch. Add parsing regressions for paths,
   ordinary text, prefixes, mixed unsupported commands and the eight-command cap.
3. In `route_inbound`, expand valid sequences into existing handlers with stable
   child IDs and preserve the parent receipt on failed saves. Keep prepared media
   and accepted prompt payloads unchanged.
4. Run focused Rust and Python tests, Clippy and format/whitespace checks. Request
   two independent read-only reviews and fix demonstrated findings. Record the
   behaviour and limits in the unreleased changelog and reference.

Preserve the unrelated, already deployed table-formatting changes. Do not deploy,
publish, bump versions, change accounts or send live test commands in this task.

## Verification record

- Observed the ordered-sync regression fail before adding dispatch, then pass.
- Added five bridge regressions and one parser regression. They cover the
  requested sequence, ordered effects, background history, restart replay,
  partial errors, failed saves, invalid arguments and owner/plain-text checks.
- Both independent read-only reviews found no remaining demonstrated issues.
  Fixed and reproduced the review finding for invalid-sequence save rollback.
- Rust verification passed 509 tests, with eight ignored live tests. Two
  embedded-runtime tests failed under the required build-storage adapter because
  its temporary Unix socket path is too long; the remaining suite passed with
  only those two tests filtered out. No storage configuration was changed.
- Python verification passed 174 tests. Strict Clippy, formatting and whitespace
  checks passed. Logs are under `/tmp/opencode/codr-serial-*`.
- Added the behaviour to the Unreleased changelog and reference. Source remains
  uncommitted and unpublished. The installed executable was not changed.
