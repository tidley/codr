# Agent-led room installation (opt-in)

**Core compatibility is an unpublished source candidate.** The published
`v0.13.0-preview.1` is the prerequisite, not a Core-compatible package. Do not
install it on a Core host expecting this path. The compatibility candidate has
not been qualified on macOS or published; keep existing installations untouched.
Use a separately approved exact source/release ref and matching verified assets
once available. The package examples below retain the prerequisite version; they
do not authorise building, publishing or relabelling it.

## The one instruction to give your coding agent

Copy this instruction unchanged on every supported OS; fill in public details
you already know. Missing public details can be supplied when the agent asks.

> Set up the opt-in Codr shared-room path from https://github.com/tidley/codr at the owner-approved exact SOURCE_REF. Follow docs/AGENT-INSTALL.md for the detected OS and architecture. Require matching published, verified assets and read-only Herdr capability preflight before provisioning anything; if unavailable, report that limit without changing the machine. Configure existing compatible Herdr/OpenCode without changing unrelated settings or permissions, connect a fresh room-bound agent, and verify normal Enter input. My agent-owner public key is OWNER_NPUB, the target room ID is GROUP_HEX, the alias is ALIAS, the project directory is DIRECTORY, and the authorised inviter public key is INVITER_NPUB. Ask only for missing public details, genuine OS access, identity/invitation/unknown-roster or invocation choices. Never request private keys, adopt private history, replay uncertain operations, answer human permission cards, upgrade/downgrade or restart shared services. Report exactly what is installed and verified and anything still blocked.

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

### Herdr capability preflight (all supported Unix hosts)

The source path distinguishes the old TUI API from **Herdr Core hc 0.1.22,
protocol generation 1**, using read-only `rpc.discover` and `hello`. These are
newline JSON-RPC 2.0 calls to the explicit existing Core socket, not the Herdr
app's separate `do`/`ls`/`commands` protocol. Unknown, mixed or malformed
capabilities fail before creating an account or terminal. The Core version is
deliberately bounded to the pinned contract, not guessed from an app version.

```text
BINARY room-setup preflight --herdr-socket HERDR_SOCKET
```

`init` repeats this preflight before reserving a root or provisioning an identity;
`bind` checks the same backend and permanent core ID before native creation.
Legacy preview journals are TUI-only and cannot switch backends implicitly.

Core starts one local terminal with explicit OpenCode executable argv and the
fresh native session ID, without shell text, templates, parent notifications,
automatic keep/restart or a task. Discovery requires its exact argv/session label
proof. Routing identity includes permanent core ID, terminal address, creation
time, executable, directory and native session ID, never a display name. Remote
machine terminals are excluded because this contract does not expose their
permanent core identity. Duplicate addresses/session proofs fail closed. Native
ownership remains global across both backends, so switching sources cannot adopt
another transport's private/shared history.

**Readiness limit:** Core status is program-reported OSC 7501, not screen/PID
inference. OpenCode must report idle/done for fresh binding. Missing reports
remain unknown; if readiness cannot be established, preserve the pending journal,
session and terminal. This candidate does not install a status-reporting plugin
or claim that the installed OpenCode version reports it. `spawn_pending`, timeout,
malformed creation replies and connection loss never retry `terminal.run` or
native creation. Reconnect is read-only; rerunning `bind` refuses pending phases.
Input always uses the existing native OpenCode admission/adapter; Core
`terminal.send`/`terminal.keys` cannot substitute for it or answer permission cards.

**App visibility limit:** a Core terminal is not automatically an app pane. The
pinned Herdr app 0.1.18 commands document `herdr-app do terminal.keep` with the
exact `terminal` address and `place: "tab"` as the action for showing an existing
running terminal. This candidate does not execute that separate protocol or
verify its result. A receiving agent must inspect the installed app commands,
its selected core and `ls` output before any separately authorised visibility
action, then verify the pane references the same terminal. Do not use
`space.new`, `terminal.open`, `doc.set` or another launch as a visibility shortcut.
No app documents are rewritten. If the exact same terminal cannot be shown and
verified safely, report desktop visibility/normal Enter validation incomplete,
not a completed onboarding. Neither Linux fixtures nor a bound mapping establish
macOS app visibility or end-to-end readiness.

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
either a new TUI workspace/pane or one Core terminal, starts OpenCode on that
exact ID and verifies it.
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
