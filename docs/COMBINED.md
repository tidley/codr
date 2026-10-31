# Combined codr daemon (0.10.0 implementation, unreleased)

This build can host the bridge and MDK 0.14 messaging runtime in one process.
It does not spawn `wn-agent`. MDK keeps ownership of keys, account storage,
cryptography, relay transport, and invitations. The existing private
`marmot.agent-control.v2` socket remains available to local setup tools.

Build with the normal `cargo build --locked` command. The three patched MDK
crates and their license are in `vendor/mdk`; other MDK crates use the existing
immutable source pin. The combined binary uses MDK's SQLCipher dependency.
This does not authorize opening older or incompatible account storage.
The repository pins Rust 1.97.1, matching the MDK build cohort.

## Chat-only upgrade from 0.9

This path is implemented but not released. Do not expect the published 0.9
release to offer an unpublished 0.10 build.

1. In 0.9, send `/update` and use the confirmation code that version displays.
   Its unchanged helper installs the signed 0.10 executable in bridge-only mode.
   It does not restart messaging or convert services without separate consent.
2. After the bridge update completes, send `/update` again. The 0.10 offer
   explains the local handover of the installed executable. Confirm with
   **`/update confirm`**, without a code. No second release download is needed.
3. The independent helper verifies both legacy services, account identity and
   file-backed credentials. It stops both services and verifies a private cold
   backup before it changes startup configuration. It includes account files,
   credentials, SQLite sidecars, routing state, configuration and service files.
4. The helper converts the existing worker service in place. It retains its old
   name so unchanged plugin startup scripts start the same combined process.
   The external connector stays stopped. Voice dependencies remain in place.
5. Read the result in the original chat, then check `/status` from the phone.
   Keep the private backup. Never restore old MLS state after newer traffic.

The new executable retains the external-mode `update-info` contract understood
by the old helper. New helpers use `update-info --full` for the declared combined
runtime and storage cohort. Release downloads still require pinned signatures,
checksums and independent HTTPS assets. A local handover does not invent a
release record or permit same-version executable downloads.

Custom service commands, unknown overrides, different relays, ambiguous accounts,
symlinked storage, external credential stores or storage-cohort changes fail
closed. They require inspection and the manual procedure below. A failed
handover restores service definitions, not an older database or receipt ledger.
An uncertain result never authorises an automatic retry.

## Manual existing-installation handover

Do not run both installations against the same account home. Do not run the
release installer to obtain this unpublished preview: released 0.9.0 assets
do not contain it.

1. Stop the bridge and external connector during an agreed maintenance window.
2. Make a private, device-local backup of the complete connector home, bridge
   state, configuration, service definitions, and old executable. Include SQLite
   sidecars and private credentials in that backup; never export them to chat.
3. Confirm that the account home already uses the pinned MDK 0.14 cohort. A
   different storage cohort requires explicit migration approval.
4. Keep the old executable as `previous` in the private backup. The handover
   helper expects `config.toml`, `state.json`, `mdk-home/` (all stopped home files),
   and `services/` (old unit/plist files) alongside it. Back up secret-store
   credentials privately as well; do not export them to chat. Old services must
   be stopped **and disabled** before the helper runs. It does not stop them for you.
5. With the reviewed combined executable installed, deliberately run:

   ```sh
   python3 scripts/setup.py --combined-home /absolute/path/to/existing-mdk-home \
     --backup /absolute/private/device-local/backup \
     --previous-connector /absolute/path/to/previous/wn-agent \
     --approve-handover
   ```

   This is a live service action and requires explicit approval. It checks the
   old cohort, saved identity, stopped/disabled jobs, matching backup files and
   absent socket owner before installing one `codr` service. Config, chat state
   and account data are not rewritten. If startup fails, only the combined job
   is stopped/disabled for inspection; old MLS state is never restored.

   The direct foreground form remains available for isolated fixtures:

   ```sh
   herdr-white-noise-bridge --config /absolute/path/config.toml run \
     --messaging-home /absolute/path/to/existing-mdk-home
   ```

   The home must exist. This command does not create an account or identity.
   An active external control-socket owner is refused. Socket access remains
   same-user and private, with no debug/allow-any invitation policy.
6. For systemd, `systemd/codr.service` is the one-service template. Set its
   arguments to your existing paths. Disable the old two services only as part
   of the approved handover; this document does not start or stop them.
   For launchd, use the same executable and arguments in one LaunchAgent.
7. Verify the running binary, original account, group mappings, native sessions,
   queues, and actual phone round trips. A relay acknowledgement alone does not
   establish phone receipt.

SIGTERM and Ctrl-C request orderly runtime shutdown. Do not restore an old
account snapshot after newer MLS traffic as a binary rollback. Reuse the live
account home with the previous compatible runtime after stopping this one.

## Confirmed whole-binary updates

After the approved handover, send `/update`, then **`/update confirm`**. No typed
code is needed. The offer stays owner-only, chat-bound and valid for ten minutes;
old or unverifiable transport timestamps cannot confirm it. The updater validates
the actual managed executable, configuration and messaging-home argument.

One replacement updates both embedded components. MDK implementation changes may
update within the explicit `storage_cohort` contract in `connector.toml`; maintainers
must change that contract before incompatible storage changes. Protocol/state or
cohort changes are refused. Readiness requires the configured local account to
respond, not merely a running bridge. Failed startup rolls back the executable,
not the account database or routing receipts. A relay response does not prove
phone receipt; a live phone round trip remains a separate deployment check.

## Current limits

- This is an opt-in local preview. Existing default startup and released
  installation/update paths remain in external-connector mode.
- The immutable 0.9 bridge-only updater cannot perform a layout conversion in
  its first step. The new 0.10 external worker offers a second, explicit chat
  confirmation. Unsupported layouts still require the manual handover.
- This MVP reuses existing 0.9 account homes. Fresh identity setup still uses
  local 0.9 setup first; a new one-binary account-creation wizard is not included.
- Messaging runs on its own runtime threads. A failed messaging start is
  reported without terminating desktop service. This is not process-level
  crash isolation.
- The existing bridge still has messaging waits in its event loop, and desktop
  queueing still requires a mapped White Noise group. Removing those contracts
  is a separate part of the architecture change; do not claim full desktop
  independence from this first combined-runtime preview.
- Combining processes does not itself fix device-package selection or prove
  invitation delivery. The separate approved local combined rollout does not
  verify this new automated handover. Its tests use isolated fixtures only.
