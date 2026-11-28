# Pinned public contracts

These are complete, unchanged public reference files, not machine snapshots.
Read their method definitions and lifecycle contracts when reviewing Core support.
The OpenRPC document is used by the isolated Rust socket fixtures; the protocol
notes explain program-reported status and uncertain spawn recovery. The app
commands describe a separate CLI, not additional Core JSON-RPC methods.

| File | Public source | SHA-256 |
|---|---|---|
| `schema.json` | https://raw.githubusercontent.com/herdrdev/herdr-core/v0.1.22/schema/hc.openrpc.json | `e4f018b6b7b11f9b18a1a6bc8d6fb48c89d57e67c930f11bca8da6a068abeeb4` |
| `protocol.md` | https://raw.githubusercontent.com/herdrdev/herdr-core/v0.1.22/schema/README.md | `669240f145352b8ef8069b2679c23581d14d1917f06688e4b2bfedd45599ad1f` |
| `app-commands.md` | https://raw.githubusercontent.com/herdrdev/herdr-app/v0.1.18/docs/commands.md | `3609ca249a10e1cc0d365c83ae6fe7d87463df43a199f1dcc71e3b92e1571cbb` |

Tests use fake local transports only. They do not prove an installed OpenCode
reports OSC 7501, adapter hotload, macOS runtime behaviour, or app-pane visibility.
