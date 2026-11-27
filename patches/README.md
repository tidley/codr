# Connector source

**codr** ships a dedicated `wn-agent` built from the MDK revision in [`connector.toml`](../connector.toml).
The release workflow applies [`mdk-codr.patch`](mdk-codr.patch) before building it.

The current pin is stable MDK 0.19.0 at `3fce95465cdea1edc8456dbd1c6691d1c8dc1f2e`.
Both full releases and Linux previews build the connector from this source.
The patch is rebased on that exact release. Founding-admin controls are now
upstream; Codr retains its opt-in media-reference and graceful-host-shutdown extensions.
The patch also adds authenticated room account/message context and external-only
human account login, signer registration and encrypted sends. These use the same
MDK revision and storage cohort. See [shared-room boundaries](../docs/SHARED_CHATS.md).

Before the first 0.19 start, stop every process using the account and keep a private, device-local
copy of the connector home, including database sidecars, encrypted media, configuration,
and secret-store credentials, but excluding `accounts/*/handoff/` plaintext leases.
MDK advances the account schema to 218 on first open;
older binaries cannot open the upgraded data. Do not roll back only the executable or automatically
restore an old account snapshot after new MLS traffic. See the upstream
[0.18 migration guide](https://github.com/marmot-protocol/mdk/blob/v0.19.0/docs/integration/0.18.0.md#storage-upgrade)
and [0.19 guide](https://github.com/marmot-protocol/mdk/blob/v0.19.0/docs/integration/0.19.0.md).
Upgrades from 0.14 must also account for the linked 0.15–0.17 guides.
Keep the account directory out of cloud and device-sync backups.

The release supports founding admin selection through `group_create_with_admins`; the patch retains the local `include_media`
history-recovery extension, including available parented thread messages. Existing `group_create` clients keep creator-only defaults. Non-member admins are refused
before creation. The engine assigns the owner and creator roles in the founding state, including the initial Welcome.

The bridge probes `group_create_capabilities` before recording creation intent, so an older connector can be upgraded
without leaving a false in-flight creation marker.

Upstream includes the founding-admin protocol and relay-backed tests. The local
patch includes media-default and deleted-message regressions. The connector archive contains the upstream license and source
provenance. The installer verifies its checksum and uses a versioned, **codr**-owned path. It does not replace a shared
`wn-agent` executable.
