# 0.10.0: one executable with whole-application phone updates

## Confirmed product requirement

Tom requested this for **0.10.0**, not 0.9.0:

- Combine the Codr bridge and MDK messaging runtime in one executable and one
  managed service on Linux and macOS. Do not merely package two executables
  together or spawn a separate `wn-agent` child.
- Keep the owner-facing `/update` command and explicit confirmation. An update
  must replace the complete combined executable, updating both parts together.
- Confirmation is **`/update confirm`**, with no typed code. Retain owner/chat
  binding, a fresh ten-minute offer, transport-time replay protection and one
  durable global installation intent. Stale or unverifiable confirmations fail
  closed; the UI change does not authorize automatic installation.
- Preserve account identity, credentials, chats, native sessions, history,
  accepted/uncertain deliveries and sync settings. Verify safe restart and
  binary rollback without restoring stale MLS account state.
- Retain signed release discovery, independent downloads, checksum/size checks,
  version and storage compatibility checks, and original-chat completion replies.
- Cover both systemd and launchd, including the existing macOS startup fix.

## Current state and boundaries

The local combined-runtime preview embeds MDK but disables phone updates in
combined mode. It is not yet the requested deliverable. Its updater, installer,
service handover, packaging and recovery contracts need completion and testing.
Existing local work must be audited and preserved, not merged wholesale.

0.9.0 remains the separately built bridge plus connector. Its `/update` replaces
only the bridge executable and its embedded helpers, not the connector.
This 0.10.0 request does not expand the authorized 0.9.0 release scope.

The first 0.9.0-to-combined handover needs an explicit compatibility design.
Do not assume that the old bridge-only updater can perform it. A service-layout
change or MDK storage migration must not be hidden inside an ordinary binary
update. Any live handover requires an agreed stop, private device-local backup
and explicit approval before account storage is opened by an incompatible runtime.
No live handover, storage migration or combined-runtime publication is authorized
merely by recording this requirement.

## Acceptance checks

1. One installed executable and one service; no separate connector process.
2. A confirmed phone update replaces both embedded components and restarts the
   same managed service, without changing identity or losing accepted work.
3. Cancelled, stale, mismatched, corrupt and incompatible offers install nothing.
4. Startup failure restores the previous compatible executable and preserves live
   messaging state; uncertain operations are not replayed automatically.
5. Verify source and regression tests, two independent read-only reviews, all four
   platform packages, and an explicitly approved live phone round trip before
   describing the single-binary update path as ready.
