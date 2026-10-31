# MDK sources used by the combined codr daemon

Source: <https://github.com/marmot-protocol/mdk>, revision
`3fb4b4759ac676b0c21c4c3527074866c9fa4c89` (MDK 0.14.0), MIT license.

These three crates contain codr's existing `patches/mdk-codr.patch` changes.
Other MDK crates remain Git dependencies pinned to that same revision.
The workspace manifest changes only dependency paths and workspace membership.
The copied HPKE provider and root dependency patches preserve MDK's existing
security and SQLCipher backports.

Additional local change: `agent_connector::serve_socket_until` accepts a host
shutdown future, cancels connection tasks, and calls MDK's
`shutdown_and_close` before returning. Existing standalone `serve_socket`
retains its API and delegates with a never-completing shutdown future.

Do not replace this source with a newer MDK revision as an implicit update.
Opening existing account data requires the normal stopped-backup and explicit
handover policy. This task does not authorize a storage migration.
