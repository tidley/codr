# Connector source

**codr** ships a dedicated `wn-agent` built from the MDK revision in [`connector.toml`](../connector.toml).
The release workflow applies [`mdk-codr.patch`](mdk-codr.patch) before building it.

The current pin is MDK 0.17.0 at `4df811aea99269b437a6023be9446a5141e93cbe`.
Both full releases and Linux previews build the connector from this source.
The patch is rebased on that exact release. Founding-admin controls are now
upstream; Codr retains its opt-in media-reference and graceful-host-shutdown extensions.

Before the first 0.17 start, stop every process using the account and keep a private, device-local
copy of the complete connector home, including database sidecars, media, configuration,
and secret-store credentials. MDK advances the account schema on first open;
older binaries cannot open the upgraded data. Do not roll back only the executable or automatically
restore an old account snapshot after new MLS traffic. See the upstream
[migration guide](https://github.com/marmot-protocol/mdk/blob/v0.17.0/docs/integration/0.17.0.md#storage-upgrade)
and its linked 0.15/0.16 guides when upgrading from Codr's previous 0.14 pin.
Keep the account directory out of cloud and device-sync backups.

The release supports founding admin selection through `group_create_with_admins`; the patch retains the local `include_media`
history-recovery extension. Existing `group_create` clients keep creator-only defaults. Non-member admins are refused
before creation. The engine assigns the owner and creator roles in the founding state, including the initial Welcome.

The bridge probes `group_create_capabilities` before recording creation intent, so an older connector can be upgraded
without leaving a false in-flight creation marker.

Upstream includes the founding-admin protocol and relay-backed tests. The local
patch includes media-default and deleted-message regressions. The connector archive contains the upstream license and source
provenance. The installer verifies its checksum and uses a versioned, **codr**-owned path. It does not replace a shared
`wn-agent` executable.
