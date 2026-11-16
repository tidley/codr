# MDK sources used by the combined codr daemon

Source: <https://github.com/marmot-protocol/mdk>, revision
`4df811aea99269b437a6023be9446a5141e93cbe` (MDK 0.17.0), MIT license.

These three crates contain codr's existing `patches/mdk-codr.patch` changes.
Other MDK crates remain Git dependencies pinned to that same revision.
The workspace manifest changes only dependency paths and workspace membership.
Root dependency patches preserve the release's hax and SQLCipher backports.
The obsolete local HPKE 0.7 provider is removed: upstream now uses its pinned
OpenMLS/RustCrypto HPKE 0.8 provider.

The published Codr patch includes `agent_connector::serve_socket_until`, which accepts a host
shutdown future, cancels connection tasks, and calls MDK's
`shutdown_and_close` before returning. Existing standalone `serve_socket`
retains its API and delegates with a never-completing shutdown future.

Founding-admin controls and their validation are now upstream. The remaining
local protocol extension is opt-in media references on timeline-message lookup.
The patch includes its wire/default and deleted-message regressions.

The declared storage cohort is `mdk-0.17`. First opening an older account upgrades
its schema; downgrade is unsupported. Opening real account data requires explicit
migration approval and a verified stopped backup. Updating this source does not
authorize that migration. Never restore old MLS state after newer traffic.
