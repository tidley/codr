# Shared-room identity and access (#34)

This source candidate implements the accepted shared-room identity/access design.
It is not installation, native Teem qualification, or live-room acceptance.
Runtime setup, signing, accounts, membership changes and service operations need
separate approval. Do not run setup against retained private sessions.

## Isolation and setup

Each local target uses a **dedicated bridge transport account/configuration**, one
existing encrypted room, and one fresh, empty OpenCode V2 conversation. Other
targets use their own transport accounts and state files. Do not reuse a private
mapping, transport state or native history. Keep the existing single-storage-owner
and cold-backup gates when using a combined connector.
The dedicated configuration must have `control_chat = false`: a pre-created
private control mapping makes the transport state ineligible for attachment.

**Attach offline, before starting this transport's discovery worker.** The running
worker discovers private sessions until a room binding exists. Initial attachment
through a running Porter endpoint therefore races private mapping/group creation.
Do not delete retained mappings or retry uncertain creation to bypass this check.

With the dedicated worker stopped and its approved connector already available,
use the existing offline command. Replace the placeholders with the dedicated
configuration, exact Herdr key (the JSON tuple `[source, agent, kind, value]`),
canonical group hex and stable lowercase alias:

```sh
herdr-white-noise-bridge --config /absolute/path/room.toml \
  attach-room '["SOURCE","AGENT","KIND","VALUE"]' \
  --group GROUP_HEX --alias builder
```

This command locks the dedicated state and checks authenticated membership, empty
native history and native ownership before saving. It does not enter the discovery
loop, create a session, invite anyone, or send a room message. It does not start an
embedded connector: observe the existing storage-owner and backup gates when
making the configured connector available. Do not open a second storage owner.

Only after attachment succeeds, start the dedicated worker with Porter enabled
under the separately approved runtime procedure. Use the same configuration and
private local host interface with `version: 1`, the exact `bridge_account`, and
`session_key`. These are host requests, not room messages:

1. `room.roster`: supply `roles`, mapping **every authenticated account ID** to
   `human` or `agent`. The owner must be human; the local transport must be agent.
   The connector reads account IDs from MDK's MLS account roster, not leaf counts.
2. `room.grant`: supply `account`, `role`, and explicit boolean `allowed`.
   Human and agent grants are independent, directional and initially empty.
   **Even the owner needs an invocation grant.** Membership grants reading only.
3. Optional `room.agent_budget`: set `enabled` explicitly. Enabling alone does not
   supply a budget: a newly accepted human message starts a four-agent cycle.
4. Inspect `room.status`. `room.stop` persists a stop and clears the agent budget;
   `room.resume` removes the stop without creating a new budget.

The same-user private host endpoint remains the setup/control boundary. Room
senders cannot call it. The phone-facing `/agents` commands below are a separate,
owner-only exception for invocation grants, not access to private host commands.
Changed or incomplete account classification disables invocation until the owner
reconciles the full current roster. Removed or reclassified accounts lose grants.
An observed roster change persists a hold, even if the previous member set returns
or the worker restarts. Grants, resume and budget controls cannot clear that hold;
the owner must explicitly reconcile the complete authenticated roster.

Updated managed transports reserve native conversations across configurations in
the UID's fixed passwd home under `.codr-native-rooms`, not a configurable state
directory. Attachment and admission use the same per-conversation lock and durable
ownership record. A copied state file, changed source/terminal or another transport
cannot take over that reservation. Missing, partial or conflicting records fail
closed; they do not expire and have no automatic reset or adoption. A failed state
save can leave a reservation without a mapping. Preserve both records and inspect
the recorded owner, transport, state path and native identity before any separately
approved recovery; do not delete reservations or mappings to force attachment.

These reservations coordinate updated managed Codr transports. They cannot inspect
unknown offline legacy state files or force older binaries and arbitrary native
clients to honor the lock. Select a genuinely fresh dedicated conversation, never
one used by another client or private transport. Unexpected out-of-band input still
quarantines the room binding.

A dedicated room transport does not discover unrelated sessions or accept new
private/shared bindings. Legacy `attach-shared-chat` alias-only bindings remain
retained for identity/history ownership, but are quarantined and cannot invoke or
forward output. They are **not** an alternate access path. Do not delete them to
bypass attachment checks. Private `attach-chat` remains separate and unchanged.

## Authenticated addressing and finite interaction

### Explicit desktop relay (source-only)

`room.desktop_send` is a same-user Porter host operation. It publishes the explicit
desktop instruction with the label `Desktop via Codr`, signed by
the existing **Codr transport account**, not by the owner's White Noise identity.
The label is display-only. It does not create a human signature, recipient tag,
invocation grant, or tool approval. No phone key or external signer is needed.

Use the running dedicated room transport's configuration and exact native key
and terminal ID. Keep one UUID per instruction:

```sh
herdr-white-noise-bridge --config /absolute/path/room.toml \
  room-send 'NATIVE_KEY' --terminal-id TERMINAL_ID \
  --request-id UUID 'How is the weather?'
```

The CLI uses the existing Porter socket, not another worker or storage owner.
Its JSON equivalent uses `version: 1`, `method: "room.desktop_send"`,
`bridge_account`, `session_key`, `terminal_id`, `request_id`, and `text`.
The socket's same-UID access authenticates this explicit local owner action.
The owner's invocation grant, classified current roster, resumed room, unchanged
native ownership and an available agent are still required. These checks run
before publication and again before native admission. The room post must be
confirmed before the instruction is submitted. A later safety failure can leave
a visible instruction **without** native admission; publication alone is not an
acceptance receipt. Correlated replies use the existing labelled room outbox.

Reusing the exact UUID and request returns its recorded result without another
publication or prompt. Reusing the UUID with different content or identity is
rejected. An unfinished operation stays uncertain across restart and is never
automatically replayed. If a request times out or reports uncertainty, inspect
the retained receipt and native state; do not generate a new UUID to retry it.

The CLI is an explicit send path. Ordinary room conversation remains non-invoking.
Existing held sessions are not recovered or rebound by this command. Source
changes do not install or activate the feature.

### Normal agent-pane input

The optional `adapters/opencode-room-input` OpenCode V2 plugin connects normal
agent-pane Enter to this same relay. It runs the supported **prompt-admission
hook**, before OpenCode stores a user message or starts model work. Its private
binding names exactly one already-attached room session; unrelated sessions are
unchanged. No OpenCode core patch, service restart, phone key or human signer is
needed. Plugin/config installation remains a separately authorised runtime action.

Load the local package in that project's OpenCode configuration:

```json
{
  "plugins": [{
    "package": "/absolute/path/to/codr/adapters/opencode-room-input",
    "options": { "binding_file": "/private/room-input.json" }
  }]
}
```

The owner-only (mode `0600`) binding file contains `sessionID`, `key` (the exact
native key), `terminal`, `account` (Codr transport), `group`, `statePath` (the
dedicated room state) and `socket` (its Porter control socket). It contains no
signing secrets. State and binding reads require private, same-UID regular files;
the final file must not be a symlink. Each JSON read is limited to 4 MiB.

For normal text, the hook calls `room.desktop_prepare` with the existing native
message ID and a stable request UUID derived from session/message IDs. Codr checks
owner invocation access, roster, stop, privacy and native ownership, publishes
`Desktop via Codr`, then durably records the exact native ID/text fingerprint.
The hook queues the **original** native message after a confirmed preparation
receipt. It never submits a second prompt. Optimistic terminal working detection
does not prevent preparation; blocked permission/form states still do.

Hook or transport failure rejects native admission. There is no terminal fallback
or automatic publication retry. A prepared receipt is not proof of native
acceptance: cancellation or a later hook failure can leave a visible room message
without an admitted prompt. Repeating preparation for the same native ID rechecks
current access without republishing or renewing the four-invocation budget.
Attachments, selected skills, agent mentions and bridge commands are rejected by
this text-only adapter rather than silently stripped or exposed.

Phone/CLI inputs already reserved by Codr must not reenter the blocked bridge
worker from inside its native POST. The adapter skips that hook path only for a
`msg_codr_` ID whose exact fingerprint and room admission are already durable in
the bound state. A prefix, display label or caller metadata alone cannot bypass
it. Unregistered native input still quarantines the room; existing holds remain
unchanged. Correlated replies use the existing room output route.

Run the isolated adapter fixtures with `bun test adapters/opencode-room-input`.
They use local fake sockets and files, not live sessions, models or room accounts.

The connector resolves each message by account, room and message ID. It returns
the authenticated sender, text, account roster and signed inner-event `p` tags.
Inbound envelope text, client roles, display names, `mentions_self` and raw
`@alias` text are not invocation authority.

When the roster has two or more **human accounts**, every invocation needs a
signed `p` tag for the target transport account, including owner messages. Multiple
agents alone do not impose this requirement. Clients must support authenticated
account addressing; typing a mention is insufficient. Replies retain `@alias`
labels for display only and are visible to room members.
Normal forwarded agent replies do not manufacture signed recipient tags from
their text. An agent-origin invocation must itself arrive with authenticated
account addressing from a compatible sender; a reply that merely names an alias
cannot start an agent cycle in a multi-human room.

Agent invocation also requires an explicit agent grant and an enabled budget.
At most four agent messages are accepted per local target between newly accepted
human messages. Duplicate humans, refused commands, restart, enabling and resume
cannot renew a cycle. Each target independently bounds a non-cooperative cycle.

Shared messages never enter the private command dispatcher. Slash commands,
unsupported media and blocked native sessions cannot invoke. Invocation grants
do not permit launching sessions, reading private history, changing permissions,
answering native forms or approving tools. Handle native approvals locally.

## Owner-only phone access controls

The authenticated owner of each local target can manage its existing invocation
grants from the room. This works without an owner invocation grant, while the
target is busy/stopped, and without a working native model backend. It does not
resume the target, renew the four-agent budget, cancel accepted work, or grant
tool/application permissions. Group admins, other humans and agents have no owner
authority, even if they can invoke the target.

Send `/agents @builder`, replacing `builder` with the local target's exact alias.
Alternatively, send `/agents` with authenticated account addressing for that
target. An untargeted `/agents` is ignored rather than waking every room agent.
Aliases select a target only; the signed sender account proves owner authority.

The reply lists the target identity and each classified peer's human/agent grant,
plus a `Revision:` token. Copy that token and the member's full account hex or
`npub` into one of these explicit commands:

```text
/agents @builder allow MEMBER_ACCOUNT REVISION
/agents @builder revoke MEMBER_ACCOUNT REVISION
```

There are no numeric menu indices. The revision is bound to the owner, target,
room and current grant/classification revision. Each grant update invalidates
older choices, including host-side updates and grant/revoke round trips. Replies
to successful changes include the new revision. If a choice is stale, request
`/agents @builder` again. Replayed message IDs never reapply or acknowledge a
control, including after restart. Old attachment-baseline traffic is not accepted.

Only the authenticated owner can request these replies or change grants. The
replies themselves are ordinary encrypted **room messages**, visible to room
members; this is not a confidential owner-only inbox. Unauthorized requests get
no menu or acknowledgement. Shared commands never enter private command handling
or the native prompt/form/permission paths.

Phone controls manage only currently classified members. A changed, held or
unclassified roster produces an instruction to reconcile through the existing
local `room.roster` setup operation. No human/agent role is guessed or granted
from a name, leaf count, admin flag, client role or text mention. Revocation stops
future admissions; previously accepted native work is not cancelled or replayed.

## External human accounts

Humans send under their own accounts through MDK's external-signer lifecycle.
There is no relabelled bridge send, human secret import, or local-key fallback.
The private host interface accepts:

- `human.login`: `human_account`, `signer_socket`, and a new UUID `request_id`.
  Explicitly invokes MDK external-account login/setup with default options.
- `human.register`: the same fields, for an existing external-only account after
  connector startup. Registration does not convert a local-key account.
- `human.send`: `human_account`, `group`, `text`, `addressed_accounts` (canonical
  member IDs), and a new UUID `request_id`. MDK encrypts and sends as that human.

All requests also include `version: 1` and the configured `bridge_account`.
Intent is persisted before delegation. An identical completed request returns its
saved result. Reusing its UUID with different content fails. Refusal or uncertainty
survives restart and is never automatically replayed.

The signer socket is an absolute Unix socket in a same-user private directory.
Both directory and socket must deny group/other access; the connected peer must
have the same UID. The pinned public key is checked at registration and signed
events must preserve the requested event ID, identity and valid signature.
The bounded JSON-line protocol uses `{version:1,id,method,params}` and a matching
`{version:1,id,result}` or error response. Supported methods are `get_public_key`,
`sign_event`, and MDK's NIP-04/NIP-44 encrypt/decrypt operations. Each request has
a 120-second bound. Refusal, timeout, identity changes and malformed responses
fail closed. This adapter is not an Amber/NIP-46 daemon; supplying a compatible
approved local signer endpoint is a separate runtime prerequisite.

## Delivery, recovery and quarantine

Roster and message context are checked again immediately before native dispatch.
Deduplication, budget consumption, exact native message ID and input fingerprint
are saved before the one native POST. Uncertain admission pauses new prompts;
reconciliation is read-only and never posts a retry or terminal fallback. A failed
reader before submission preserves the event for authenticated recovery. Old
messages without timestamps newer than attachment cannot become new prompts.

Accepted native IDs remain bound to input fingerprints after receipts clear.
Unexpected native input, changed text under an accepted ID, or changed native
identity quarantines the binding. Queued room output is removed, and flush refuses
quarantined room delivery. Quarantine has no automatic reset: inspect the retained
native identity/history under separately approved recovery. No private history is
copied into the room. Existing global forwarding controls still govern delivery.

## Source and verification

The additive host/protocol changes use the existing pinned MDK 0.19.0 revision and
storage cohort. `patches/mdk-codr.patch` includes them for external connector
builds; the vendored subset carries the same source. No new protocol database is
introduced. Human account/login and encrypted sends have not been exercised with
live storage, signers, identities, relays or rooms.

Fixtures use isolated Unix/HTTP/signature peers, not live models. Focused checks:

```sh
cargo test --locked --test bridge shared_chats -- --test-threads=1
cargo test --locked --test room_access
cargo test --locked --test external_human
```

Run all root gates too. Two fresh independent read-only reviews of the frozen
candidate remain required. Source tests do not establish live encrypted-group
acceptance or authorise publication, account provisioning, deployment or restart.
