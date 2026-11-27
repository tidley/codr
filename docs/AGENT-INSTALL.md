# Agent-led room installation (0.13.0-preview.1, opt-in)

## The one instruction to give your coding agent

Copy this instruction unchanged on every supported OS; fill in public details
you already know. Missing public details can be supplied when the agent asks.

> Install the opt-in Codr shared-room preview from https://github.com/tidley/codr at v0.13.0-preview.1. Follow docs/AGENT-INSTALL.md for your detected OS and architecture, install necessary dependencies and the verified package, configure my existing Herdr/OpenCode without changing other settings or permissions, connect my fresh room-bound agent, and verify normal Enter input. My agent-owner public key is OWNER_NPUB, the target room ID is GROUP_HEX, the agent alias is ALIAS, the project directory is DIRECTORY, and the authorised inviter public key is INVITER_NPUB. Ask only for missing public details, genuine OS access, identity/invitation/unknown-roster or invocation choices. Never request private keys, convert private history, replay uncertain operations, answer human permission cards, or restart shared services. Report exactly what is installed and verified and anything still blocked.

The owner and inviter can be **different people**. An inviter/admin can admit the
transport, but cannot grant access to another person's agent. Desktop posts are
signed by Codr and labelled **Desktop via Codr**, never signed as the human.

## Receiving agent: common contract

1. Detect OS and native architecture. Do not ask the human to choose shell
   commands. Inspect existing Herdr/OpenCode, plugin registration, service files,
   Codr account homes/cohorts, mappings and running readers. This is **fresh setup
   only**: refuse a retained or ambiguous Codr installation. Do not migrate,
   downgrade, unlink plugins, stop other jobs, or change their permissions.
2. Read this guide from the exact selected, clean release checkout. Verify its tag
   and commit and refuse modified source/build inputs. The release must
   exist, have matching version/source evidence and the host archive/checksum.
   Unpublished/missing platform assets are a stop, not permission to build a
   replacement or use 0.12.2 bytes. Do not use `latest` for a preview; changed
   source requires artifacts built from that exact release source.
3. Use existing Python 3.9+ and Git. The binary installer needs **no Rust, Cargo,
   Node, Bun, npm, Homebrew or Homebrew Python 3.11**. If a dependency is genuinely
   missing, use the platform's supported installation mechanism with its normal
   OS access gate; never bypass it. Herdr/OpenCode must support native V2 session
   APIs, project plugin hotload and the prompt-admission hook. Inspect their
   installed help/API documentation; do not invent channels, ports or flags.
   Do not upgrade or restart an existing incompatible shared runtime silently.
4. From the pinned checkout run `python3 scripts/install.py`. Previews install
   under `~/.local/share/codr/packages/v0.13.0-preview.1/`; stable executable,
   services and stable update channel remain untouched. Installer verifies the
    archive SHA-256, exact adapter/source files and source commit against checkout,
    executable digest and native object-header architecture, then its compiled
    version/source/target identity. Release packaging requires clean committed
    source; arm64 Mach-O codesigning remains required. Preserve an existing package instead of
   overwriting it. Retain the checkout for source evidence and installer access.
5. Use the matching platform path below. Human invocation choices are separate
   from room admission and from native tool/OS permissions. No default grants,
   even for the owner; no default agent budget.

This change supplies the fresh **macOS** helper. Linux's pre-existing private-chat
setup is **not** a shared-room fallback. Windows runtime/packaging is a separate
candidate: do not claim it installed until its platform guide/assets are present
in the selected release. On an unsupported or absent platform path, report that
specific limitation without taking accounts or services live.

## macOS: steps for the agent, not a human command list

Use the installed package's `herdr-white-noise-bridge` as `BINARY` and its directory
as `PACKAGE`. Resolve physical paths first (`/var` and `/tmp` may be symlinks).
Select an unused, short, owner-controlled persistent `ROOT` (at most 60 bytes,
for example a new direct child of the user's physical home). Do not put persistent
accounts in `/tmp`. No existing empty root may be adopted. The helper refuses
standard legacy configuration/account/service paths; inspect custom paths and
loaded launchd jobs too. Never stop unrelated jobs to make this path pass.

Discover the **explicit existing** Herdr socket and authenticated local OpenCode
service registration from their native configuration/API. Resolve the OpenCode
executable and existing project directory physically. The service file must be a
private owner-controlled regular file with loopback HTTP URL, PID and nonempty
password; never print its contents or pass credentials on a command line. No
guessed port, unauthenticated service, global `/api/location/reload`, #541 patch,
or OpenCode restart is part of this path.

The following are argv templates for the receiving agent to populate and execute
with correct argument quoting, **not commands for the human to assemble**:

```text
BINARY room-setup init --root ROOT --owner-npub OWNER_NPUB --target-group GROUP_HEX --alias ALIAS --directory DIRECTORY --authorised-inviter INVITER_NPUB --herdr-socket HERDR_SOCKET --opencode-service-file SERVICE_FILE --opencode-bin OPENCODE_BINARY
```

`init` reserves the root and writes durable intent before opening MDK, creating
the transport identity, publishing its KeyPackage or configuring invite policy.
It reuses `EmbeddedMessaging` and connector `run_bootstrap`, but first requires an
explicit **empty account list**: upstream bootstrap's normal account reuse is not
permitted. It runs messaging only, **never bridge discovery**. Printed npub and
nprofile are public invite details. No phone/human private key is needed.

Ask the authorised inviter to send the real signed invitation to that exact
transport identity. The allowlist authenticates the **welcomer**, not the target
group. It does not substitute for an invitation or constrain the inviter to one
room. Other accepted groups are not bound or discovered. Then read:

```text
BINARY room-setup roster --root ROOT
```

If an invite has not arrived, this read-only step can be repeated. It briefly
hosts only messaging and requires the **exact accepted group/account**, owner
membership and authenticated account roster. Show every full account ID to the
owner. Obtain their explicit `human`/`agent` classifications for **all** accounts;
never infer roles from names, npubs' appearance, admin status or leaf counts. The
owner must classify as human, this transport as agent. Store those choices in a
JSON object `ROLES_FILE`, keyed by full account ID, then execute:

```text
BINARY room-setup bind --root ROOT --roles ROLES_FILE
```

`bind` persists native intent and a durable pending room reservation **before
creating the native session**. It then creates a fresh empty native V2 session and
a new Herdr workspace/pane, starts OpenCode on that exact ID and verifies it.
Strict attachment keeps the original guard, verifies empty history and atomically
finalizes only that guard's reservation. A crash leaves a permanent hold that even
the same owner cannot adopt by rerunning setup. Ordinary attachment cannot adopt
pending reservations. This prevents private discovery during interrupted setup.
Private mappings/history are never converted. New-pane shell-busy is retried only
when Herdr explicitly proves no input was submitted; all other uncertain effects
remain pending and cannot be replayed. A changed roster stops setup. Binding
starts with empty invocation grants and disabled agent budget.

Choose **one** mode under the owner's existing installation authority: a new
dedicated user launchd job or explicit foreground execution. This is not consent
to replace existing jobs. Install the adapter/project configuration and emit the
chosen launch specification:

```text
python3 PACKAGE/scripts/room_setup.py --root ROOT --package PACKAGE --mode launchd
```

For explicit foreground use `--mode foreground` instead. The helper verifies
packaged adapter hashes, saves one-shot intent, preserves all existing JSON
project settings/MCP/permissions, adds only this adapter entry, and writes its
private exact-session binding. Existing JSONC, linked or conflicting adapter
configurations are refused for a separate preserving edit, not overwritten.
The helper **does not start a service**. Under the selected mode, execute its
returned argv (foreground) or returned `launch_argv` (new dedicated launchd label).
The job has no automatic crash retry; inspect an uncertain exit before restart.
No systemd on Mac, no global plugin configuration, no shared runtime restart.
Project hotload is the only activation mechanism; if unavailable, stop.

## Invocation decisions and acceptance

Use the owner's authenticated phone `/agents @ALIAS` flow documented in
[SHARED_CHATS.md](SHARED_CHATS.md#owner-only-phone-access-controls), or explicit same-user
`room.grant` after an actual owner choice. **Do not grant the owner automatically.**
No agent budget is enabled unless the owner explicitly chooses it; its maximum
remains four accepted agent invocations between accepted human inputs. Changed
rosters, stop holds, deduplication and uncertainty rules remain in force.

After the owner permits invocation, verify normal **Enter in the exact bound
session**, with an owner-chosen benign instruction. Confirm its labelled Codr room
post, native admission and correlated reply, and that unrelated native sessions
remain unchanged. Multiple human accounts still require authenticated addressing
for phone invocation. Keep tool/OS permission cards for their real human recipient.
Do not replace normal input verification with a synthetic room-send call. An
uncertain receipt is not success and must not be retried with a new ID.

Report separately: package/source checks, configured binding, adapter hotload,
chosen job/foreground status, owner choices, and actual end-to-end evidence. Linux
fixtures are **not Mac runtime proof**. Installation does not imply source review,
publication, CI, native Teem qualification or a verified mirror.

## Recovery

Preserve `setup.json`, all pending phases, account storage, native sessions/claims,
mapping state, configuration backups and adapter/service intents. Only roster reads
are repeatable. Do not rerun `init`, `bind` or adapter installation after uncertain
effects, delete an empty/failed root, allocate another identity/session as a retry,
or erase a claim to force attachment. Diagnose the recorded exact operation read-only
and report the bounded recovery decision; this preview has no automatic migration
or repair command.
