# **codr** reference

[Overview](../README.md) · [Installation guide](INSTALL.md)

Detailed behaviour, operating controls, recovery, and known limits.

The bridge uses one dedicated MDK identity across all mapped groups. Setup keeps its account separate from other connectors.

## Behaviour

- New sessions create groups and invite the configured phone `npub`.
- Group names pair the **herdr** tab label and workspace: `<tab name> (<workspace>)`, for example `review (Acme)`. Fresh sessions launched from an existing chat add the selected agent inside the parentheses, such as `review (Acme - codex)` or `review (Acme - claude)`, without renaming the original chat. Without a workspace label, these fresh chats use `review (codex)`; ordinary chats keep only `<tab name>`. The agent suffix survives restarts, pane moves, and metadata updates, and is kept when long titles are shortened. Workspace and agent details also appear in the group description. Repository names and agent-generated conversation topics do not determine the chat name.
- A phone message prompts only the agent mapped to that group.
- Complete owner text is handed to the agent immediately, even while it is working, without an interrupt. A blocked native dialog requires its supported answer or `/cancel`. Media and previously accepted deliveries remain durable; `/queue` inspects them. `/clear-queue` is retired and does not discard accepted work.
- Final replies come from completed stored agent messages. They exclude reasoning and tool output.
- Working, idle, blocked, and ended notices appear in the group. Blocked notices can include a labelled terminal excerpt. Unsupported OpenCode blockers use a short notice. Separate `done` notices are suppressed; the final reply signals completion.
- `/status` reports the mapped session state.
- `unmute` or `/unmute` resumes forwarding for all chats without sending a prompt or answering an agent question.
- Ended sessions retain their group. A restored native session reuses its group.
- Each newly discovered native session gets its own chat automatically, including on a reused terminal. The original chat, history, receipts and queued work stay bound to the original session. Restoring a known native session reconnects its existing chat. Missing or duplicate identities still prevent discovery. Group names follow tab and workspace labels, not an agent's optional nickname.
- A plain SSH shell does not register the remote agent with local Herdr. If SSH replaces an agent in its pane, the original chat loses its local agent connection even when a remote agent UI remains visible. Codr does not guess the remote identity, forward local session hooks over SSH, or start another agent automatically in an occupied pane. Keep the local controller and interactive SSH terminal in separate panes. Restoring the exact original native session reconnects its existing chat without changing its history or delivery records. A different remote native session must not silently inherit that chat.
- Images and audio use the media route below. Other attachment types and arbitrary terminal key commands are not supported.
- Other detected agent backends get groups, prompts, and state notices. Structured final-reply extraction supports **OpenCode, Codex, and Claude Code**.

**herdr** snapshots refresh every three seconds. Very short intermediate states can occur between snapshots. Native session references preserve routing across pane moves. Group creation waits for **herdr** to report a native session reference. During identity uncertainty, the bridge defers prompts and preserves the existing group.

The worker reads session transcripts in background jobs, with at most four reads in parallel and one outstanding read per session. Each OpenCode snapshot supplies replies, tasks, and decisions together. Completed reads wake the worker without waiting for slower sessions. State changes and terminal submissions remain serial. Reads from an older session state or forwarding mode are discarded. An exiting session gets a fresh final transcript read before its mapping closes.

The four-reader limit bounds background I/O, not chat subscriptions. The bridge listens for phone messages across all mapped chats.

This removes serial transcript exports from the main loop. **herdr** control requests, MDK delivery, and history recovery still await their own I/O. Parallel reads do not guarantee a fixed phone-delivery time.

### Start a workspace from your phone

New bridge accounts use the display name **Pat**. New session chats introduce Pat as the messenger between White Noise and herdr and direct you to `/help`. Verify the account's full `npub`, not its display name. Setup preserves existing profiles and account keys. See [Pat onboarding](INSTALL.md#pat-your-bridge-identity).

The phone controls below do not require the optional **portr** desktop/SSH endpoint. That endpoint is disabled unless `portr_enabled = true` is configured or the worker is explicitly started with `run --portr`. See [optional installation](INSTALL.md#optional-portr-desktopssh-access).

Send `/new` in any linked chat or the control chat (below). The plugin guides you through **existing or new herdr workspace → folder name → installed agent → model → confirmation**. The four selection headings use bracketed progress: `[1/4] Choose a herdr workspace:`, `[2/4] Folder name:`, `[3/4] Choose an installed agent:`, and `[4/4] Choose a model for OpenCode:` (or the selected agent). The title is outside the brackets. Each prompt explains how to reply:

1. Existing workspaces are listed first. The final numbered choice is **Create new workspace**, followed by **0. Cancel**. With two workspaces, reply `3` to create a new one. Labels and stable IDs distinguish workspaces with the same name. Use `next` or `previous` for ten-item pages; numbers are global across pages, and the new-workspace number is always one higher than the total workspace count. Page counters appear only when there is more than one page. `/new workspace 1` and `new` also work.
2. Reply with your chosen folder name, such as `my-idea`, or `0` to cancel. Reply with **`ls`** to list selectable directories under the launch root, 200 per page, and choose one by its displayed number. **`ls <part>`**, for example `ls api`, filters folder names by a case-insensitive substring. All matching folders remain available through **`next`** and **`previous`** pages; numbers refer to the saved filtered list and selections must belong to the displayed page. A new filter starts at page one. Bare **`ls`** clears the filter, refreshes all folders and returns to page one. `ls next` searches for folders containing `next`; bare `next` changes pages. `/new folder my-idea` remains supported. Existing conversations are reused as described below. For a new conversation, a new workspace uses the folder's final component as its name, or an existing workspace receives a new project tab with an explicit cwd. Existing tabs and workspace names stay unchanged.
3. Reply with a listed number to choose an installed agent: OpenCode, Codex, or Claude Code. `/agent 1` also works.
4. Model selection starts with **Backend default**, when supported, then provider groups such as **View OpenAI / Codex models**, **View Claude models**, and **View Free models**. Select a provider/category to receive its **full available model list**, with no pagination. This also applies when only one provider is available. Variants appear only after selecting a model that has them, as a separate full list. Use `back` to return to the preceding model menu. Long lists use the existing encrypted message-size limit and are delivered in consecutive messages without dropping options. `/model 2` also works. A default-only catalogue shows just its default choice. Older saved flat menus keep their original numbering and pagination until you start a fresh selection.
5. The complete preview shows the workspace destination, full folder path, agent, model, and whether the folder exists or will be created. It says **Confirm within ten minutes**. Reply `confirm`, `yes` or `y`, or type the displayed **repository folder name**, not the workspace label. For `tools/repo.v2` inside workspace `Main`, type `repo.v2`, not `Main`. Reply `0` to cancel. A wrong name stays within confirmation and creates nothing. Destination workspace renames require a fresh preview and confirmation. After expiry, the next plain reply closes the confirmation and reports expiry. Later messages follow normal chat routing. `/new confirm` remains supported, including for a repository folder named `0`.

Workspace selection does not create anything. If the existing-workspace list is unavailable, the menu says so: retry with `/new`, or explicitly choose a new workspace. Before launch, the plugin checks the selected existing workspace again. If it was renamed, a new confirmation is required. If it is gone or unavailable, launch stops without creating a folder or using another workspace. There is no automatic fallback to a new workspace.

Guided `/new` checks existing folders for conversations reported by Herdr or retained in codr's mappings. One match directs you to its existing White Noise chat; several matches show a numbered conversation menu. No chat mappings, histories, or queues are replaced. A closed or changed conversation requires local reattachment, not a new session. If Herdr cannot be checked, setup stops rather than assuming no conversation exists. Native histories never observed by Herdr/codr are not searched. The check runs again before a new launch is confirmed. Explicit fresh-session commands such as `/agent` retain their existing behavior.

No long code is needed. Short answers apply to the current menu in this chat, which survives restarts and expires after ten minutes. All editable setup menus and pending confirmations offer **0. Cancel**. While a selection menu is open, numbers belong to that menu, not to an agent's pending decision. While the folder prompt is open, plain text is treated as a folder name; while a launch confirmation is waiting, it is treated as a confirmation name. Slash commands such as `/help` still work. To answer the agent instead, cancel setup first with `0` or `/new cancel`. Ordinary text and numbers without a setup menu keep their existing behavior. Explicit coded commands remain supported for compatibility and reject stale codes. `/new cancel CODE` cancels a particular launch, and `/new status` inspects the last launch. Starting a new configuration invalidates the previous unconfirmed launch. Uncertain or in-progress operations still require local inspection and cannot be cleared by `0`.

The one-line `/new my-idea my-idea opencode` shortcut remains available, creates a new workspace, and uses the backend's configured default model. Guided selection detects executable installations in the worker's environment; installation does not prove backend authentication. The configured OpenCode executable must use the same service/profile as the OpenCode executable launched by **herdr**.

Folders are restricted to `launch_root`, which defaults to `~/code`. The root must already exist. To override it, set an absolute path such as `launch_root = "/home/your-user/projects"` in the bridge configuration. Relative paths, absolute paths, and `~/` paths must all resolve below that root. Folder names cannot contain spaces, hidden components, traversal, or symlinks. Agent arguments and arbitrary commands are not supported.

Existing folder contents are kept. Missing folders are created only after confirmation. If the folder's identity or existence changes after the preview, a new confirmation is required. Launch controls do not change forwarding mode or existing prompt queues. Captions, voice transcripts, and **portr** queue messages cannot confirm a launch.

The workspace opens without stealing focus, and the agent starts in its project directory. No initial prompt is sent. Guided OpenCode V2 launches first create a native session with the selected model and location, then attach the terminal to that session. Codex and Claude receive only the selected `--model` argument; arbitrary arguments are not accepted. The normal White Noise invitation follows after **herdr** reports the agent's native session. Backends that do not publish a native session until their first interaction may require that interaction locally.

Requests survive worker restarts. If a workspace or startup acknowledgement is lost, no automatic retry runs. `/new status` shows the recorded phase and any confirmed workspace ID. Inspect **herdr** locally before recovery. Keep partial folders and workspaces; this command never deletes them. An interrupted or uncertain request blocks a replacement in that chat. Local recovery requires inspecting the recorded request; there is no phone-side force-retry command.

### Agent and model selection in existing chats

- `/agent` opens the installed-agent menu for the mapped project. Selecting an agent and model produces a fresh-session confirmation. On confirmation, **codr** opens a new tab in the same workspace and eventually creates a separate White Noise chat. It does not stop the old agent, transfer its history, move its prompts, or remap its chat. The existing project must be below `launch_root`.
- `/model` in an OpenCode V2 chat lists models for that project's location. After selection, `/model confirm` changes the model for subsequent turns in that same native session. It keeps the chat and history. Confirmation requires a live idle session with no waiting or in-flight bridge prompts, decision, or permission request. A busy session is not interrupted; confirm again with a new message once it is idle.
- `/model` in a Codex or Claude chat opens model selection for a **fresh session in a separate chat**, clearly stated before confirmation. In-place switching for these backends is not supported. The old session and queue remain unchanged.

OpenCode models come from its authenticated V2 `/api/model` catalogue, filtered to enabled, non-deprecated choices. Provider IDs and available variants distinguish otherwise similar names. **OpenAI / Codex** is the OpenAI provider group inside OpenCode, not a switch to the Codex agent. The **Free models** group requires explicit zero input, output, and cache prices in the API; absent prices are not evidence that a subscription model is free. The catalogue uses the target folder's configuration, or the nearest existing parent for a new folder. Codex choices come from its local `models_cache.json` and are labelled **cached**; an absent cache leaves only the backend default. Claude choices are its documented `sonnet`, `opus`, and `haiku` aliases, not a verified account-entitlement list. Backend authentication and account access can still prevent startup.

Selected models and installed agents are checked again before a launch. The plugin never changes global backend settings. It saves intent before a model switch and reads back the selected model. An unconfirmed switch is not repeated, even after a restart, and requires local inspection. Selection commands do not change forwarding mode. Captions, voice transcripts, truncated messages, other senders, and **portr** queue input cannot operate these controls.

### Control chat

Set `control_chat = true` in the bridge configuration and restart the worker. On the next start, the bridge creates one owner-only White Noise group, invites the configured phone, and posts a short welcome. It is not mapped to an agent session; it exists so `/new` does not have to be sent inside a session chat.

The control chat accepts `/new`, `/help`, `/status`, `/recent`, `/queue`, `/sync auto|on|off`, and `/unmute`. The leading `/` is optional there, and unambiguous prefixes work (`new`, `n`, `st`, `sy`); ambiguous words list the candidates instead of guessing. `/status` reports the mapped-session count and the current sync mode. Plain text gets a short instruction instead of prompting an agent. `/agent`, `/model`, and `/cancel` describe the currently mapped session, so they are refused there; use `/new` to start a project, then continue in the new chat.

Creation uses the same non-retry rule as session groups: if the group-create response is uncertain, the bridge records `creating` and reports that inspection is needed. `herdr-white-noise-bridge status` prints the control entry with `"control": true` so `resolve-create control --group GROUP_HEX` can be used after inspecting the phone. The control group keeps its fixed name and is skipped by automatic and manual renames.

### Incremental OpenCode database reads

Set `opencode_db` to the absolute path of the database used by the mapped OpenCode sessions. For a default Linux profile, this is usually `~/.local/share/opencode/opencode.db`. Expand `~` before adding the path to the configuration:

```toml
opencode_db = "/home/your-user/.local/share/opencode/opencode.db"
```

**codr** opens that database read-only and uses short SQLite read transactions. It does not change OpenCode tables, indexes, journal settings, or migrations. WAL mode lets OpenCode continue to write during a read.

The first read loads the selected session once. Later reads use the indexed per-session event sequence and fetch only messages affected by new events. Updates to existing text, tasks, and questions are included. Unchanged sessions read no message or part bodies. Deletions remove cached content. A restart, replaced checkpoint, or event-log gap rebuilds the cache once. Durable reply IDs still prevent replay after a restart.

This reader supports the v1 `message`/`part` projections and durable message-event format verified with OpenCode 1.19.3. If required projection columns, the session's durable event log, or a supported message-event version are unavailable, **codr** logs the compatibility change and uses CLI export for that session. This also applies when OpenCode upgrades while the bridge is running. The configured `opencode_bin` must export the same mapped sessions. CLI exports support OpenCode 1.x and 2.x, but full exports cost more than incremental database reads.

Missing sessions in a compatible database, unreadable or corrupt databases, locks, and malformed content remain errors. They do not select another reader or consume pending phone input. A failed export also leaves the history checkpoint pending. Forwarding resumes only after a successful checkpoint; muted history is not replayed.

Setup validates live supported session histories before it changes accounts or services. If no supported session is live, it reports that validation is deferred. To repeat the read-only check manually, run:

```sh
~/.local/bin/herdr-white-noise-bridge check-readers
```

The result reports the number of checked session readers. It does not print transcripts or change bridge state.

### At desk / Away forwarding

By default, a new native user prompt in OpenCode, Codex, or Claude Code mutes **all groups**. A new authorized owner message from your phone enables forwarding for all groups after the history checkpoint. Phone prompts are attributed through durable message IDs and prompt fingerprints, including across bridge restarts. Ambiguous or unconfirmed input is treated as local input and mutes forwarding. Historical input on first observation is only a baseline.

Detection uses transcript polling, not keystrokes. Allow the three-second polling interval plus transcript read time. Output already submitted to MDK cannot be recalled. Do not use automatic handoff as an instantaneous privacy boundary. Set `auto_forwarding = false` in the bridge configuration to disable local-prompt detection and retain the previous manual controls and phone wake behavior.

If a native message appears before its text parts, outgoing delivery pauses until attribution is possible. If that session ends with incomplete input, **codr** mutes globally instead; a new authorized phone message can resume the other sessions.

Use the **herdr** action **White Noise forwarding: At desk / Away** to open the control popup. **Toggle White Noise forwarding** switches modes directly and can be bound to a **herdr** shortcut.

**herdr** 0.9.1 does not list plugin actions in its main click menu. For quick access, add this to your **herdr** configuration:

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "local.white-noise.forwarding"
description = "White Noise: At desk / Away"
```

With the default prefix, press **Ctrl+B**, release it, then press **f** without Shift. The shortcut also appears in **Keybinds** under **White Noise: At desk / Away**. **herdr** reloads configuration changes live.

Remote **herdr** clients use their local key bindings by default. Add the binding on the client machine, or reconnect with `herdr --remote <host> --remote-keybindings server`. This does not require a server restart. You can also run `herdr plugin pane open --plugin local.white-noise --entrypoint forwarding --focus` on the remote host.

Startup no longer opens the popup automatically. Use the shortcut or action when you need manual control.

- **At desk:** suppress outgoing chat messages while the bridge continues to listen for your phone. New group invitations and chat-name/description updates remain active.
- **Away:** forward progress and final replies from every mapped session.
- In automatic mode, a new message from the configured phone to any mapped group switches **all sessions** to Away, except `/help`, `/recent`, `/last`, sync/queue controls, `/cancel`, and `/new` launch controls. Already processed messages, your bridge's own messages, and other senders do not switch modes.

To resume forwarding without asking an agent to do work, send `unmute` or `/unmute` by itself in any linked White Noise chat. **codr** consumes the command and confirms in that chat after the normal history checkpoint. It works in busy, blocked, and ended conversations. It does not start a turn or answer a pending decision. Recognition ignores capitalization and surrounding whitespace; longer messages, attachment captions, voice transcripts, and truncated input are not commands. Only the configured phone owner can use it. Muted history is not replayed, and duplicate deliveries cannot unmute again after a later mute.

In the popup, enter **1** for At desk, **2** for Away, **3** to toggle, or **q** to close. Press Enter after your choice. Muting applies to every mapped session; it does not stop the transport or change your groups.

The selected mode survives restarts. Mode changes discard the automatic outgoing backlog, but preserve explicitly requested `/help`, `/recent` and `/last` replies and the edit targets of already-attempted progress messages. Muting pauses those edits; resuming reuses the same progress bubble or finishes it as `done` if a completed reply was recorded while muted. Before Away becomes active, the bridge checkpoints replies for all active sessions. The popup shows **Away requested** while a transcript checkpoint is pending. Completed muted turns remain suppressed even if their transcript records arrive late. An operation already submitted to MDK can still arrive after you select At desk.

Phone sync overrides are persistent: `/sync on` means **Always on**, `/sync off` means **Always off**, and `/sync auto` restores normal automatic behavior. Local activity, phone wake-up, and the At desk/Away controls do not change a forced mode. `/unmute` clears it and requests automatic sync with all chats unmuted. `/help` and bare `/sync` show the current mode. Always off suppresses automatic outgoing messages but does not hold new phone text from reaching the agent; explicit control replies and `/recent` still work. Returning to an enabled state retains the history checkpoint before outgoing sync resumes.

The command-line controls work while the worker runs:

```sh
~/.local/bin/herdr-white-noise-bridge forwarding status
~/.local/bin/herdr-white-noise-bridge forwarding desk
~/.local/bin/herdr-white-noise-bridge forwarding away
~/.local/bin/herdr-white-noise-bridge forwarding toggle
```

Forwarding defaults to Away on an existing installation. The control file is private and separate from routing state, so changing modes does not stop either service.

### White Noise chat commands

Send these as standalone text from the configured owner's account in a linked chat:

| Command | Result |
| --- | --- |
| `/help` | List commands and the current sync mode in this chat only. |
| `/update` | Check stable ngit releases and offer a chat-bound confirmation before updating the managed bridge worker. |
| `/recent [hours]` | Send up to three conversation text messages, labelled You or Agent with section breaks, to each chat active in the past hour or the specified number of hours. `/recent2` and `/recent 2` both select two hours. Does not resume sync. |
| `/last [count]` | Replay the newest conversation message in this agent chat, without changing sync. `/last4` and `/last 4` replay the newest four; counts 1–100 are accepted. |
| `/sync auto\|on\|off` | Set a persistent sync mode. Bare `/sync` shows the current mode and choices. |
| `/status` | Show connection, agent state and pending delivery count. Resumes sync in automatic mode; a forced-off override remains off until `/unmute` or a new sync-mode choice. |
| `/queue` | Show this chat's waiting prompt count and in-flight or uncertain submission state without changing mute settings. |
| `/cancel` | Interrupt the current task in this chat through native herdr control, keeping history and accepted deliveries. Does not change sync mode. |
| `unmute` or `/unmute` | Clear a forced mode and resume automatic sync for all chats without prompting an agent. |

Commands accept **unambiguous prefixes**: `/n`, `/ne`, `/st`, `/sy`, `/q`, `/c`, `/h`, `/un`, `/up`, `/m`, `/ag`, `/al`, `/an`, `/rec`, `/rej`. Ambiguous prefixes (`/s`, `/a`, `/r`, `/u`) are listed in the control chat and left untouched in session chats, where they reach the agent. In the control chat the leading `/` is optional (`new`, `n`, `status`); bare words remain normal prompts in session chats and are not treated as commands while a launch menu is open.

Control replies wait until the host clock passes the latest owner input's whole-second timestamp in that chat. This avoids timestamp ties and small clock differences that can place a reply above its request. The deadline survives restarts. Other chats and native agent input do not wait for it. Timestamps more than five seconds ahead do not extend the deadline, so an incorrect sender clock cannot hold control replies indefinitely. Larger clock differences still require clock correction; codr never changes device clocks or rewrites message timestamps.

`/help` and `/recent` do not prompt agents, answer questions, create groups, or change sync mode. They work while muted or **herdr** is disconnected. Recently ended mapped conversations can participate in catch-up. The command chat receives a count of queued conversations and unavailable readers; a queued send does not prove phone receipt. Retrying the same inbound event does not resend the catch-up. A new `/recent` request skips unchanged source messages already confirmed sent or queued by the bridge, including previous catch-ups and ordinary assistant forwarding.

The cutoff defaults to one hour before the `/recent` request and is fixed for the whole batch. For example, `/recent` at 16:00 selects chats active since 15:00; `/recent4` or `/recent 4` selects chats active since 12:00. The number is a positive whole number of hours, not a message count. Zero, negative, fractional, extra or overflowing arguments receive usage guidance without prompting an agent. The last confirmed outgoing-message clock is retained for delivery accounting, but does not extend this activity window.

`/last` reads only the current chat's saved native conversation, including You and
Agent text, not tool output or command acknowledgements. It sends available
messages in source order, with sender and source-date labels. It has no activity
cutoff and deliberately includes already delivered text. A fresh request can
replay the same text; replaying the same inbound event does not duplicate it.
Replay replies do not consume automatic delivery keys, unmute chats, change a
forced sync mode, submit an agent prompt, or answer a pending question. They use
the manual outbox and can be delivered while muted. The control chat has no native
conversation and receives guidance to use an agent chat instead. Empty or
unavailable history receives a short notice. Use complete standalone text without
attachments or truncation; zero, fractional, extra or out-of-range counts receive
usage guidance. At most the newest 100 conversation messages are retained for
replay, while `/recent` continues to use its existing three-message excerpt.

Each excerpt message has a **You** or **Agent** label followed by its source date
and time, for example `2026-10-28 12:13:14 UTC`. OpenCode uses its message creation
clock, with completion time as a fallback. Codex and Claude use their transcript
timestamps. Dates are normalized to UTC. Missing or invalid timestamps show
`Date unknown`; neither file modification time nor the current time is used as a
replacement. Date labels do not change source identities or resend known messages.

Delivery accounting survives restarts and mode changes. Older installations without a confirmation clock recover the newest available own chat message from retained MDK history, paging past incoming messages when needed. Unavailable readers are reported without blocking other chats' excerpts. MDK confirmation does not prove receipt on the phone.

Conversation activity comes from source clocks, not polling: OpenCode message creation, streaming/update, and completion clocks, or the verified Codex/Claude session log's modification time. Histories with no known activity time, future activity times, no conversation text, or activity before the shared cutoff are skipped. A failed reader does not prevent successful readers from contributing.

The time window selects conversations, not individual excerpt messages. Each eligible conversation contributes its last three available user or completed assistant text messages in order, even if some context predates the cutoff. This is often Agent → You → Agent, but it follows the actual conversation and can end with an unanswered user prompt. Empty messages, attachment-only inputs, tool output, internal summaries, private subagent branches, and unfinished assistant replies are excluded. Each excerpt is grouped into one labelled catch-up message; very long excerpts use continuation messages. No model call is made. Sync does not change normal reply checkpoints or replay all missed messages. Catch-ups use durable, idempotent delivery across restarts.

Within that three-message excerpt, already delivered or pending messages are removed without backfilling older history. Delivery records are per chat and survive restarts. A source message is confirmed only after all of its batch's chunks receive valid MDK acknowledgements; failed or partial sends retain their queued retry keys. An identical text with a different source ID, or edited content under the same ID, is a new message version. Incoming phone messages are not themselves outgoing confirmations. Delivery tracking begins with this update: older sends without source records cannot be marked confirmed reliably and may appear once in the first catch-up after upgrade.

Recognition ignores capitalization and surrounding whitespace for standalone controls. Longer prose, attachment captions, voice transcripts, and truncated input cannot operate these controls. Use a listed number, an exact option label, or `/answer 2` for a supported pending decision; include the question code when its card requests one.

### Confirmed updates from ngit

Send `/update` as standalone owner text. In combined mode a background, read-only check selects
the newest stable numeric codr release published by the pinned maintainer:
`npub19at4nqjjymca2lh3v546vcg5ktun3al4c5xu2pts3ewckv0y70ys0gupzr`.
It uses the existing ngit repository and the author's `codr` application records.
The host needs ngit v3 and Git in its service PATH. No Nostr signing credentials
or GitHub access are needed for this command. Release signatures are checked by
ngit; the updater also checks the pinned author/application and asset metadata.

In 0.10, the offer displays the running and available versions. Reply with
`/update confirm` within ten minutes, with no code, or `/update cancel`.
The confirmation's transport timestamp must be newer than the offer and not
implausibly future-dated. Missing/stale timestamps refuse installation. A
confirmation from a previously replaced offer cannot use its old transport time.
Released 0.9 workers still require the confirmation code shown by that version.
Their first 0.10 update keeps messaging external. In a 0.10 external worker,
`/update` instead offers a local cold-backup/service handover of the already
installed executable. Confirm it separately with `/update confirm`. Standard
managed installations do not need terminal commands for this second step.
There is no installation from plain numbers, another chat, attachment captions,
voice transcripts, or another participant. Confirmation and the selected release
event/hashes survive restarts. Equal/older executables are never installed.
The local same-version service handover is a distinct action with no release or
download. It retains the old worker service name for legacy startup hooks, and
sets `CODR_SERVICE_NAME` for future updates of that same combined service. A new
check replaces that chat's previous offer. A restart during the read-only check
may require another `/update`; it never starts an installation by itself.

After confirmation, an independent per-user systemd or launchd job revalidates
the pinned release event. It downloads this platform's bridge archive and its
checksum from independent HTTPS Blossom URLs, verifies both signed hashes and
sizes, and checks the archive checksum and expected regular executable. GitHub
URLs and redirects are rejected, with no fallback. macOS signature validation
uses the existing installer. Missing, incomplete or conflicting records leave
the running executable unchanged.

Only the standard `~/.local/bin/herdr-white-noise-bridge` installation supervised
by a verified per-user service is supported. The candidate must expose
`update-info --full`, match the confirmed version, protocol and state/storage cohort,
and read the existing configuration/state successfully. External-connector mode
still requires the exact connector pin/patch. Combined mode updates implementation
revisions within the explicitly declared cohort and restarts the same verified
service with the verified messaging home. Chat handovers retain
`herdr-white-noise-bridge`; manual combined setup can use `codr`.
It replaces bridge and messaging code
together. The supported external-to-combined layout conversion has its own
explicit chat confirmation and private cold backup. Other layout/cohort changes
require local setup and its backup/migration approval gates. Herdr's coding sessions are not restarted and accepted work is
not cleared or automatically replayed.

The updater keeps the old executable and a stopped, private state backup under
the routing state's `.updates` directory. It replaces the executable atomically,
starts the same service, and requires a version/job/PID-bound startup receipt.
In combined mode, the existing local messaging account must also respond before
that receipt is written. A slow first launch has a bounded 120-second window.
Failed startup restores the old executable and verifies its restart. **Rollback never
restores an older routing or MLS account state**, which could lose receipts,
repeat work or discard newer messaging traffic.
If rollback cannot confirm startup, local intervention is required.

One installation may be pending globally. `/update cancel` cannot interrupt an
already confirmed installation. An uncertain scheduler acknowledgement is not
automatically retried: inspect the `codr-update-*` systemd job or launchd job and
the private `request.json`/`result.json` records before recovery. Completion or
rollback is reported in the original encrypted chat. Missing completion does not
prove failure or authorize a second install.

### Immediate text and persistent pending deliveries

An ordinary complete owner text prompt goes directly to the freshly verified mapped agent, including while it is working. It does not wait in codr for idle or interrupt the current task. The agent may still use its own native inbox. A blocked native dialog is reported without silently making a future task; answer its supported decision or permission request, or use `/cancel`. Duplicate inbound events do not submit again. Phone text may reach the agent before an older media item finishes preparation.

Image preparation and legacy text/desktop deliveries share 16 accepted slots per chat and a 30000 UTF-8 byte caption/text limit on acceptance. A full preparation FIFO or oversized caption gets a rejection notice, not silent eviction. An image message occupies one slot, keeps its caption, and accepts up to four PNG, JPEG, WebP, or GIF attachments, with the existing 25 MiB-per-file download limit. Voice retains local transcription; it no longer waits for agent idle. New phone text has no 16-task waiting queue.

Images download and voice messages transcribe while the agent works. Once preparation completes, codr submits the caption/image path or transcript immediately, without waiting for a completed reply or interrupting the agent. Image preparation remains FIFO; older legacy text deliveries do not hold it up. Outgoing sync-off does not block prepared owner media. A native blocking dialog or unavailable/changed session still defers delivery safely. References and private prepared paths survive worker restarts. A processing failure reports that no image prompt was sent and releases only that failed item. Missing offline download references remain pending for recovery. The retired `/clear-queue` command returns a retirement notice and does not delete accepted images or text.

ZIP archives, documents and other non-audio attachments also download while the agent works. The agent receives the caption, a private local file path and the quoted original filename. The bridge does not extract archives, execute files, render active content or require transcription for ordinary files. Remote filenames are never used as destination paths. Up to four files are accepted per message, with a 25 MiB-per-file limit; regular-file, size and download MIME checks still apply. Prepared file paths and duplicate-delivery receipts survive restarts. File preparation errors are reported separately from audio/transcription failures.

Previously accepted legacy text and desktop deliveries run independently per chat using the existing guarded dispatch path. These non-media deliveries still require the same native session, an idle readiness check, enabled sync and a supported transcript reader. An early idle event alone is not completion. Muting pauses these retained deliveries without discarding them; ending a session never reroutes them to another session. This retained path does not hold new phone text or ready media.

The bridge saves consumption and submission intent before native input. A proven pre-submission socket failure retains the message for retry without clearing a running task. A lost acknowledgement, uncertain result, or restart during submission is never automatically repeated. Inspect the terminal, then use local `clear-prompt` to release a retained-delivery hold; it does not resubmit the uncertain task. `/queue` inspects pending state while muted or disconnected. `/cancel` interrupts only a freshly verified active task, persists its receipt before submission, and never repeats an uncertain interrupt automatically.

### Editable progress and separate final replies

Set `edit_responses = true` in the bridge configuration to enable editable progress. The release-matched connector includes the required `edit_message` API.

Each response starts as one `> working...` progress message. The bridge edits that message with the current task list, when available. It sends the final answer as a separate message through the normal durable send path, with its own retry key. The bridge never edits that final answer. When possible, it marks the progress message `> done`.

Phone follow-ups and blocked-to-working transitions reuse the current editable progress bubble; they do not append standalone `> working...` notices. The same bubble becomes `done` when the response completes. Without editable responses, follow-ups still do not emit extra working refreshes. Previously sent historical notices remain in the chat. Image acceptance says `Preparing image`, not `Queued prompt`: this is download preparation, not scheduling a future task. Media preparation never clears an existing response or submission receipt.

OpenCode to-do writes, Codex plan updates, and Claude Code to-do/task tools supply structured task data. The bridge does not prompt a model to obtain progress. Message IDs and pending updates survive bridge restarts. Updates coalesce between snapshots, and edit attempts are at least two timestamp seconds apart.

Progress updates have a three-second local deadline. Final replies take priority. A failed completion edit keeps its known target and retries after restart or connectivity recovery, without blocking a queued final answer or a later request's progress. If an initial progress send lost its acknowledgement and its target remains unknown, completion retains that record for local inspection (`status` reports `unresolved_progress`) instead of replaying a send that could create a replacement bubble after transport receipts expire. Progress that was never attempted is discarded after completion. An MDK response or local readback does not prove phone delivery. Very long final replies use continuation messages to preserve all text. Progress messages are ordinary encrypted chat messages, not Nostr ephemeral events.

If **herdr** becomes idle before the agent stores its final reply, the bridge keeps the `working...` progress bubble and its checklist, continues reading task updates, and retries the final transcript read. It marks progress done only when the completed reply is collected. After inspection, `clear-prompt` can release a cancelled response that has no final record.

OpenCode 2 disables todos by default. To restore native task updates, add `"experimental": { "todo": true }` to your global `~/.config/opencode/opencode.json(c)`. This makes the `todowrite` tool available; the checklist appears when the agent uses it. **codr** does not invent a task list when the agent supplies none.

Edit support is included in the dedicated connector; the flag controls whether **codr** uses it.

### Answer decisions from the phone

When OpenCode asks one single-choice question, **codr** sends a clean question card with numbered options. Reply with **1**, **2**, or an exact option label. The answer goes to the active question, even while **herdr** reports the agent as blocked; it is not submitted as a new chat prompt.

Decision cards and answers do not require editable progress.

OpenCode V2 exports omit running questions. The bridge reads live session forms through the configured reader. For a question with a native form ID, numbered and labelled choices use the authenticated form reply API, as custom answers do. The bridge sends the selected option value, not the number as custom text. No terminal screen match or keystrokes are needed. Completed forms disappear from this read and cannot be replayed as pending questions.

The bridge checks the session and exact pending question before a reply. Legacy questions without a native form ID also need an exact visible-question check before a terminal number shortcut. If an unanswered question is replaced, the new card requires its short code, for example `qabc123 1`. Duplicate deliveries and uncertain submissions are not sent again automatically.

For supported OpenCode V2 questions that allow custom input, type a complete text answer directly, such as `v6.6.6`, or use `/answer TEXT`. The bridge sends it to the freshly checked native form through the authenticated API, not as a prompt or arbitrary terminal keys. A combined reply such as `1 v6.6.6` remains literal custom text. If the question changed, prefix the text with its current question code. Custom replies are limited to 4000 UTF-8 bytes; an answer beginning with `/` must use `/answer TEXT` so commands keep their meaning. Prepared media is not automatically submitted as custom text. Submission intent is saved before the API call, and an uncertain outcome is never automatically retried.

Legacy shortcut routing was verified against OpenCode 1.18.35's single-choice question UI with 1–9 listed options. The gallery uses OpenCode 1.19.3, but does not verify live decision routing on that version. Custom text entry for legacy forms or questions that do not expose a native custom field, multi-select/multi-question dialogs, and decisions in other backends still require the terminal. Unsupported OpenCode blockers show a short notice instead of forwarding unrelated tool output.

OpenCode V2 tool permissions can show a separate permission card. The configured phone owner can reply with `/allow CODE` for **Allow once**, or `/reject CODE`. Use the code from that card in the same chat. **Always allow** remains terminal-only. The bridge checks the session and the exact pending request before sending an API reply; it never sends terminal keys or changes persistent permission rules. Replies bypass the task queue and do not resume muted forwarding. An uncertain reply is not retried, including after restart; check the terminal instead. Multiple pending permissions, oversized requests, unsafe display text, and unavailable permission APIs require the terminal.

**herdr** 0.9.1 cannot make the final identity check and terminal input atomic. Avoid answering the same question simultaneously from the terminal and phone. After an uncertain answer, inspect the terminal before using `clear-prompt` and sending a new reply.

## Shared chats and owner-only control

Group members can discuss a project and read the agent's replies. Only messages from the phone public key in **codr**'s configuration become agent prompts. Other participants' messages are not added to the agent's context. The owner can relay a suggestion in a new prompt.

New groups make both the configured phone owner and the bridge identity admins in the founding group state. The owner can invite participants and edit the group profile as soon as the invitation is accepted. Admin rights do not grant other participants permission to prompt the agent.

This requires the release-matched connector included with **codr**. The bridge checks support before recording creation intent, then uses an explicit owner-admin creation operation. An older connector cannot silently create a group without the requested rights. Existing groups keep their current admin policy.

Renaming a White Noise group does not affect routing: **codr** uses its group ID, not its display name. A title changed outside **codr** is left alone; running `rename-groups` explicitly resets titles to the **herdr** tab labels.

The gallery uses an owner-created group, bound to an isolated demo session. Alex creates the group and invites Sam through White Noise. Sam is a scripted second demo identity. Its message appears in the chat but produces no agent prompt. Alex's next message produces one prompt and a separate reply. This does not grant additional people agent control.

## Images and voice messages

### Images from the agent

In local development builds, **codr** attaches explicit Markdown images from new completed agent replies, such as `![Screenshot](screenshots/current.png)`. Relative paths use the mapped agent's working directory. Absolute paths must remain beneath that directory or `/tmp/opencode`. Symlinks, parent traversal, remote URLs, SVG, and non-image files are not uploaded. Code examples and ordinary download links remain text. To send both dark and light screenshots, embed both with `![label](path)` rather than linking the second with `[label](path)`.

Image formats are identified from their file signatures. Limits are four images, 10 MiB per image, and 25 MiB in total per reply. Missing, blocked, unsupported, or oversized images are replaced with an `Image not attached` notice; the rest of the reply still forwards. Existing mute and history-baseline rules apply. Old replies and `/recent` excerpts do not trigger image uploads.

Before delivery, **codr** snapshots each image into a private `state.outbound-media` directory beside `state.json`. MDK encrypts the snapshot before upload. The durable outbox retains the snapshot path and delivery ID across retries and restarts. Confirmed text is not resent if its image is still pending. Snapshots are removed after a successful delivery receipt is saved, or after forwarding cancellation is saved and no retained outbox item references the image. Interrupted preparation can still leave private snapshots for manual cleanup while the worker is stopped.

The connector must start with `--media-allowed-root /absolute/path/to/state.outbound-media`. Allow **only this private spool**, not the repository, home directory, or `/tmp`. Fresh setup creates and configures it. For an existing installation, inspect its effective service command and add this flag while preserving its binary, account home, socket, relays, and other arguments. Custom service overrides still require manual inspection. The directory must exist with mode `0700` before the connector starts. No identity or storage migration is needed.

### Images and voice from the phone

Images and audio are available in **codr** 0.5.0. Voice prompts require a transcription command. Setup installs the local worker described below. The [Gemini script](#gemini-transcription) sends audio to Google instead.

**codr** asks MDK's `download_media` API to fetch and decrypt each attachment. Keys stay in MDK. PNG, JPEG, WebP, and GIF images are copied into a private `media` directory beside bridge state. The prompt includes the local file paths and the original caption. The agent needs its normal local-file access to inspect them. Files remain there for later conversation access until you remove them.

Audio goes through FFmpeg to 16 kHz mono WAV, then to a local Phonon-2 worker. Its transcript enters the same prompt route as typed text, without a confirmation step. Supported question answers also use that route. Download and local transcription add time before the agent starts.

The phone sees `> transcribing` while audio processing runs. With `edit_responses = true` and an edit-capable MDK build, **codr** edits that bubble to `> transcribed` with the transcript quoted underneath. The edit survives restarts and retries independently of agent submission. Without edit support, the transcript arrives as a separate message. Images use `> downloading`. Long transcript displays are shortened with a notice; the agent receives the full prepared prompt.

Downloads and transcription run separately from session updates, one media message at a time. Pending messages survive bridge restarts. Sender checks and session checks still apply. A preparation failure sends no agent prompt. An uncertain terminal submission is not repeated automatically.

Offline media recovery uses `timeline_message_get` with opt-in `include_media` download references. The release-matched connector includes this extension. Older connectors return only attachment summaries. **codr** defers those without consuming the message and can accept a complete live event later.

Limits are four attachments per message, 25 MiB per file, and five minutes per audio file. Combined captions, transcripts, and image paths must fit within 30,000 UTF-8 bytes. Unsupported files, truncated messages, and empty transcripts do not reach the agent.

### Local speech worker

Setup installs `fermion-research` 0.2.12 with CPU-only PyTorch and downloads Phonon-2, approximately 164 MB plus runtime dependencies. FFmpeg is required. The worker uses an owner-only Unix socket under `$XDG_RUNTIME_DIR/codr-phonon` and processes recordings locally.

On Linux, `codr-phonon.service` has a 1 GiB memory limit and no swap allowance, and the bridge starts it as a dependency. launchd has no equivalent cap, so the macOS agent runs without a hard memory limit. A healthy existing **codr** worker is reused. If the worker cannot start within the Linux limit, setup reports failure rather than raising the limit.

Fresh setup includes audio by default. Use `--no-audio` for a text/image-only installation. To add Phonon to an existing installation:

```sh
python3 scripts/setup.py --phonon
```

This preserves the identity and mappings, adds the transcription command, and restarts only the bridge when its configuration changes. Existing custom transcription commands are not overwritten. A plain setup rerun preserves the existing selection.

```sh
# Linux
systemctl --user status codr-phonon.service
journalctl --user -u codr-phonon.service
# macOS
launchctl print "gui/$(id -u)/io.github.tidley.codr.codr-phonon"
tail -n 50 ~/Library/Logs/codr/codr-phonon.err.log
```

### Gemini transcription

`scripts/transcribe_gemini.py` sends each recording to the Gemini API model `gemini-3.5-transcribe`. Use it on a host where the local worker is too slow or too large. **The audio leaves the machine and goes to Google.** Google's API terms apply to it.

The script uses the same FFmpeg conversion and five-minute limit as the local worker. The model detects the spoken language and returns a verbatim transcript in that language. It needs Python 3 and FFmpeg, with no extra packages.

1. Create an API key in Google AI Studio.
2. Put the key in `~/.config/codr/gemini-api-key`, then run `chmod 600 ~/.config/codr/gemini-api-key`. The script refuses a key file with group or other access.
3. Set the command in the bridge configuration:

   ```toml
   transcribe_command = ["/usr/bin/python3", "/absolute/path/to/codr/scripts/transcribe_gemini.py", "--ffmpeg", "/opt/homebrew/bin/ffmpeg"]
   ```

4. Restart the bridge.

The key goes in the `x-goog-api-key` request header. On failure, the script exits non-zero with a one-line reason on stderr. The reason never contains the key or the response body. **codr** discards stderr, so run the script by hand with an audio file to see the reason.

Normalised audio files are temporary and removed after each attempt. MDK manages its downloaded temporary files. Image copies stay in **codr**'s private media directory. Do not commit media, transcripts, or model files to Git.

## Supported reply sources

| Agent | Final-reply source | Default transcript location |
| --- | --- | --- |
| OpenCode | Completed text from `opencode --pure export SESSION_ID` | Managed by OpenCode |
| Codex | Successful `task_complete` / `turn_complete` events with a turn ID and final message | `~/.codex/sessions` and `~/.codex/archived_sessions` |
| Claude Code | Completed main-conversation assistant responses with `message.stop_reason = "end_turn"` | `~/.claude/projects/<project>/<session-id>.jsonl` |

The bridge reads the existing session. It does not start another model invocation, change authentication, or use terminal snapshots as final replies. **herdr** must report the native session ID.

Codex and Claude Code readers verify session identity, ignore partial records, and reject ambiguous transcript files. Claude's null stop reasons and duration markers alone are not treated as successful replies. Transcript formats are internal to the agent tools; unsupported or incomplete records are not guessed into final answers.

For custom profiles, set absolute `codex_home` and `claude_home` paths in the bridge configuration. Defaults use `CODEX_HOME` / `CLAUDE_CONFIG_DIR` when available, otherwise the locations above. Fresh setup records these paths explicitly.

Existing history is baselined when a group is first attached or reply support is first enabled for an older group. It is not reposted. Later completed replies use persistent message IDs to avoid duplicates after restart.

## Roadmap

- [ ] Place a fresh Working bubble below a queued phone message only after confirmed agent submission, not merely after receipt or queue acceptance.
- [ ] Evaluate one codr executable and service with the MDK connector embedded, while keeping messaging and agent-control modules separate.
  - Compare reliability, security, upstream support, installation, updates, testing and operational complexity before choosing the process layout. Reuse MDK rather than rewrite messaging or cryptography.
  - Let desktop/SSH access use the common agent-control core without requiring a White Noise group. Keep slow messaging operations from blocking desktop control.
  - Preserve existing accounts, chats, native sessions and accepted deliveries. Verify reconnects, duplicate prevention and failed-update rollback before replacing the current two-process setup.
- [x] Add structured final replies for existing Codex sessions.
- [x] Add structured final replies for existing Claude Code sessions.
- [ ] Add structured final replies for further **herdr**-supported agents.
- [ ] Create new agent conversations from White Noise by messaging the main worker, which starts a **herdr** session and creates its mapped group.
  - Select the **herdr** workspace, directory, agent, and model.
- [ ] Let the owner authorise additional people to prompt the same agent in a White Noise group.
  - Add owner-managed prompt permissions per group, including revocation.
  - Distinguish agent-directed requests from discussion between participants.
- [x] Make the configured phone owner an admin automatically when creating new groups.
- [x] Download inbound images through MDK and pass their local paths and captions to the mapped agent.
- [x] Convert White Noise voice messages and audio attachments directly into text prompts with a local Phonon-2 worker.
  - [ ] Add guided selection of other existing transcription services or local whisper.cpp.
  - Review the existing Rust audio preprocessing and configurable transcription pipeline in [Nostr Codex Phone](https://github.com/tidley/nostr-codex-phone/blob/main/rust/src/transcribe.rs), which already supports whisper.cpp.
  - Evaluate [whisper.cpp](https://github.com/ggml-org/whisper.cpp) and Phonon-2, as used in [Phonon Wayland Dictation](https://github.com/tidley/phonon-wayland-dictation).

## Requirements

- Linux x86_64 or aarch64 with a systemd user manager, or macOS on Apple silicon or Intel with a logged-in launchd session.
- Python 3.11 or newer for setup, Git, and `launchctl` or `systemctl`. Setup finds a newer Homebrew interpreter when `/usr/bin/python3` is too old. Rust is only needed when building from source.
- **herdr** 0.9.1 or newer, with a running local server.
- The release-matched dedicated connector, including `group_create_with_admins` and `timeline_list` in `marmot.agent-control.v2`.
- A supported agent: OpenCode, Codex, or Claude Code. OpenCode needs `--pure export SESSION_ID`; Codex and Claude Code need readable native transcripts.
- **herdr** native session references for the agents you want to control.
- Your phone must publish a compatible Marmot KeyPackage. The phone and bridge need reachable relays.

The repository does not implement MLS or Nostr cryptography. MDK owns keys, encrypted group state, relay connections, invitations, and delivery.

## Build

Build with Cargo:

```sh
cargo test --locked --tests
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

`Cargo.lock` is committed because **codr** is an application. It pins the dependency versions used by the build and installation commands.

## Install

Use the [installation guide](INSTALL.md) for complete commands and checksum verification.

Install a pinned release through **herdr**, then open its guided setup tab:

```sh
herdr plugin install tidley/codr --ref v0.6.0
herdr plugin action invoke local.white-noise.setup
```

The installation hook downloads and verifies the matching **codr** bridge and connector from GitHub Releases. Setup asks for your public `npub` and configures dedicated user services. No Rust toolchain is required for this path.

For a non-interactive installation by a coding agent, see the [agent instructions](INSTALL.md#instructions-for-coding-agents).

**herdr** starts the plugin worker through its startup hook after restore. The worker's user service keeps it alive across client disconnects. On Linux, use `systemctl --user enable herdr-white-noise-bridge.service` if it must start independently at login. On macOS the LaunchAgents in `~/Library/LaunchAgents` run at login by default.

Do not share the existing `wn-opencode` connector home. That adapter can also answer messages sent to its identity.

## GitHub builds

GitHub is primary for source, issues, pull requests, reviews, CI, and releases. Contributors do not need Nostr or ngit.
The ngit repository mirrors approved source refs and published release files. Blossom hosts the exact, unchanged release assets.
See [source and release mirror verification](MIRRORING.md). Existing ngit discussions remain available without automatic duplication or closure.

GitHub Actions runs Rust formatting, tests, Clippy, and isolated installer/setup tests on Linux, plus the Rust and Python test suites on macOS, for pushes and pull requests. Version tags build separate bridge and connector archives for Linux and macOS on x86_64 and aarch64. Linux bridges use musl; Linux connectors use native glibc. macOS binaries are ad-hoc signed. Archives and SHA-256 checksums are published to [GitHub Releases](https://github.com/tidley/codr/releases). See the [connector requirements](INSTALL.md#requirements).

The installer selects the bridge and connector release from the plugin checkout. It never silently substitutes a newer release. The dedicated connector is built from the pinned upstream MDK revision and published patch in [connector source](../patches/README.md). Each connector archive includes source provenance and the upstream license.

## Verify on the phone

1. Accept an invitation from the verified bridge identity.
2. Send `/status` and check that it identifies a connected session.
3. Send a harmless request with a fresh word. Ask for that word without tools or file changes.
4. Check that the matching **herdr** session receives the prompt.
5. Check that its completed reply returns to the same White Noise group.

A relay acknowledgement proves publication, not that the phone displayed or accepted a message. Phone verification requires the round trip above.

## Recovery and diagnostics

```sh
~/.local/bin/herdr-white-noise-bridge status
# Linux
journalctl --user -u herdr-white-noise-bridge.service
# macOS
tail -n 50 ~/Library/Logs/codr/herdr-white-noise-bridge.err.log
```

State and the pending outbox use owner-only files. Keep them on persistent storage, with the MDK account home. Do not put these files in Git.

If histories appear empty and identically named invitations arrive, stop only the bridge worker and back up its current state before changing anything. Compare native session IDs, not chat names. Reattach the original OpenCode session in its Herdr pane; its retained group mapping reconnects without a new invitation. Keep replacement sessions and groups separate. Do not restore an older whole-state file, merge histories, reset an account, or migrate connector storage as a repair.

New native identities no longer need an approval just because their terminal was reused. A saved `session_change_from` hold from an older worker still needs explicit approval. Inspect both sessions and the entry in `status`. To accept that legacy candidate, stop only the worker and run:

```sh
~/.local/bin/herdr-white-noise-bridge confirm-session-change 'CANDIDATE_SESSION_KEY_FROM_STATUS'
```

Start the worker after approval. The command checks a fresh Herdr snapshot and refuses a changed selection or terminal. Approval creates a separate chat on reconciliation; it never transfers the original group's history or accepted deliveries. To recover the original instead, reselect its native session without approving the candidate. This command needs the same local state lock as the other repair commands.

The bridge saves consumed phone-message IDs before it submits terminal input. It never retries an uncertain prompt automatically. This favours avoiding duplicate actions over automatic replay after a crash.

If a prompt result remains uncertain, inspect the live session first. Stop the worker and release the pending flag without resubmitting:

```sh
# Linux
systemctl --user stop herdr-white-noise-bridge.service
~/.local/bin/herdr-white-noise-bridge clear-prompt 'SESSION_KEY_FROM_STATUS'
systemctl --user start herdr-white-noise-bridge.service
# macOS
launchctl bootout "gui/$(id -u)/io.github.tidley.codr.herdr-white-noise-bridge"
~/.local/bin/herdr-white-noise-bridge clear-prompt 'SESSION_KEY_FROM_STATUS'
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/io.github.tidley.codr.herdr-white-noise-bridge.plist"
```

Group creation has no idempotency key in MDK 0.13.0. Connection failures before submission retry automatically. A timeout or ambiguous connector error requires inspection, including a missing KeyPackage error that MDK reports only as `app_error`.

After inspection, stop the worker. Bind the existing group or explicitly permit another creation attempt:

```sh
~/.local/bin/herdr-white-noise-bridge resolve-create 'SESSION_KEY_FROM_STATUS' --group GROUP_HEX
# Use --retry only after confirming that the previous attempt created no group.
~/.local/bin/herdr-white-noise-bridge resolve-create 'SESSION_KEY_FROM_STATUS' --retry
```

Then start the worker again. Failed sends retain their private outbox entries. Other groups continue after a group-specific send failure.

Offline recovery scans retained group history every minute and after reconnect. It uses existing rows as pagination cursors and durable message IDs for deduplication. Sender timestamps do not control eligibility. This also recovers late relay deliveries and messages from a phone with a different clock.

MDK truncates long timeline text to 8,192 characters. The bridge rejects truncated recovered prompts and asks for a shorter message.

### **herdr** input limitation

**herdr** 0.9.1 does not accept an expected native-session ID in `agent.prompt`. The bridge verifies the current terminal and session immediately before submission. A concurrent session replacement between verification and submission remains a server-API race. Avoid replacing an agent while sending a phone prompt. Atomic protection requires an upstream expected-session guard.

## Group names and completion notices

To update existing group names without changing their session mappings, stop the worker and run:

```sh
~/.local/bin/herdr-white-noise-bridge rename-groups
```

Then start the worker again. This refreshes names and descriptions without changing group IDs, clears a manual-rename lock, and enables automatic renames for groups created by older versions. Ended sessions without a stored tab label use their existing title, replacing earlier **codr** prefixes or suffixes. Unavailable historical workspace details appear as `unknown`.

Live sessions automatically update their group title and description when the **herdr** tab or workspace changes. A title changed in White Noise is never overwritten automatically. If a profile update is not confirmed, automatic renames for that group stop; run `rename-groups` to re-apply the current titles and clear that state. Sessions in tabs with identical labels and workspaces can share a display name, while their routing remains separate.

### Completion notice

Stop the worker before using its state-changing CLI. To send a notice to one mapped group:

```sh
~/.local/bin/herdr-white-noise-bridge notify --group GROUP_HEX 'The bridge is ready.'
```

Without `--group`, the command sends the notice to every mapped group. The command returns success only after MDK confirms publication.

## Screenshot provenance

### 0.7.0 stock Android captures

The current images show the stock White Noise app on a connected Pixel 5. ADB captured the physical screen. The installed package is `dev.ipf.whitenoise.android`, version `2026.10.14` (22). Its APK SHA-256 is `c959e2d1a454c1ddde574c49dd03b60d60fb33df2b4f81e5fcedf5844995c87c`. No app installation, modification, or data clearing occurred.

The fictional Alex documentation profile was reverified through its full public key in the native Share & Connect screen. Pat used a separate MDK 0.14 connector home and separate codr state. Real **herdr** 0.9.1 hosted the Garden planner demo and separate, idle native OpenCode sessions. The bridge ran source commit `c55871b6cd399b65d20ce3a2ec06019f3f1a66c0`, including the control-reply ordering fix. Normal bridge services and coding sessions stayed running.

Each scene uses a fresh demo chat and one command: `/new` for workspace choices, or `/model` for provider choices. ADB entered each command through the native composer. The real connector recorded the inputs, and codr sent the replies over encrypted messaging. Both transport timestamps and rendered positions confirm that each request precedes its reply. No UI, replies, timestamps, or message order were injected or rearranged.

The menus were cancelled after capture, before confirmation. No project launch, model application, or model prompt occurred. The captured native sessions had no model messages. These images show menus and discovery, not a completed coding task or interruption of running work. Existing demo chats and their history were kept.

The README shows unscaled conversation-detail crops, with links to the full app view for context. All four images come from 1080 × 2340 device captures:

| Image | Dimensions | Source crop |
| --- | --- | --- |
| `codr-v070-android-new.png` | 1080 × 688 | `1080x688+0+1325` |
| `codr-v070-android-model.png` | 1080 × 837 | `1080x837+0+1176` |
| Both `-full.png` images | 1080 × 2145 | `1080x2145+0+132` |

The detail crops include the complete command and menu reply, without unrelated introduction, help, or cancellation messages. Full app views remove only system status and navigation bars. Native RGBA pixel comparisons passed for every crop. No text or overlays were added. PNGs contain no text or EXIF metadata, and no private conversations appear. Automatic device rotation was restored after capture.

```text
bbe183826ed824c77eff17e3e355c74b0b61f1786a2a66cf487b1b58fae5ab58  codr-v070-android-new.png
8dc3f881038d7abd5ed2209cf60331e98e15b7ee4d8f2b8707ad63995df933ca  codr-v070-android-new-full.png
28e4bfc6153f98290d3556fb2c0187e7af0afbcc3dfec5fc8e4367215ae383e2  codr-v070-android-model.png
1d09ad1feaaff3205de2bea40629b726d1c8b860b49282100dddb24ad478c759  codr-v070-android-model-full.png
```

### Earlier end-to-end examples

The earlier gallery contains side-by-side composites of real **herdr** 0.9.1, OpenCode 1.19.3, and White Noise Android interfaces.

All seven images use the same fictional Garden planner project. The desktop replays matching history imported into an isolated OpenCode installation. The phone uses a dedicated Alex demo profile. A synthetic **herdr**/OpenCode fixture supplies agent state and scripted replies through the real **codr** bridge and an isolated MDK transport.

The captures show the previous `[cd]` name prefix, blockquote status labels, editable tasks, decisions, separate replies, images, local voice transcription, a three-member chat, and a White Noise project folder. The folder is configured manually, not created by **codr**.

The reminder design is a generated demo image. The voice clip uses synthetic speech and passes through the real Phonon-2 worker. Its actual transcript reaches the fixture as the prompt, without confirmation. Sam uses a separate MDK identity. Alex's invitation and both participants' chat messages use real encrypted group messaging. Verification checks that Sam's message causes zero agent prompts and Alex's message causes exactly one.

The phone's `1` reply continues the question fixture, not a live OpenCode dialog. The captures do not verify a live coding-model run, a real project change, or a live decision dialog end to end. Editable task captures use an MDK build with edit support. Android status and navigation bars are cropped. The images contain no private conversations or text/EXIF metadata.

## Acknowledgements

**codr** builds on the work of these projects and their maintainers and contributors:

- White Noise: the mobile messaging application used as **codr**'s phone interface, maintained in the [iOS](https://github.com/marmot-protocol/whitenoise-ios) and [Android](https://github.com/marmot-protocol/whitenoise-android) repositories.
- [Marmot Development Kit (MDK)](https://github.com/marmot-protocol/mdk): the `wn-agent` connector that provides identity management, encrypted group state, relay transport, invitations, and message delivery.
- [Marmot protocol](https://github.com/marmot-protocol/marmot): the end-to-end encrypted group messaging protocol built on MLS with Nostr identity.
- [**herdr**](https://github.com/herdrdev/herdr): the agent runtime, plugin system, and local API that **codr** uses to discover and control sessions.
- [OpenCode](https://github.com/anomalyco/opencode): the coding-agent runtime whose stored final replies **codr** currently reads.
- [Codex](https://github.com/openai/codex): the coding-agent runtime and native rollout records used by **codr**'s Codex reply reader.
- [Claude Code](https://github.com/anthropics/claude-code): the coding-agent runtime whose native session transcripts **codr** reads for completed main-conversation replies.
- [Phonon-2](https://github.com/fermionresearch/phonon): the local speech model and Fermion runtime used for audio transcription. Its model weights and runtime remain upstream software.

**codr** provides the routing bridge between these systems. The messaging protocol, encryption, transport, mobile app, and agent runtimes come from the upstream projects above.
# Non-owning OpenCode V2 polling

Set `opencode_service_file` to the local V2 server registration file, for example
`/home/YOUR_USER/.local/state/opencode/service.json`. This selects authenticated,
loopback-only HTTP reads instead of CLI exports for bridge snapshots and history.
The bridge does not start, stop, replace, or write the server registration. Failed
reads retry through the normal bridge poll loop, without a CLI fallback. Start or
recover the service through its normal owner when necessary.

The reader checks a bounded durable log watermark. Unchanged history is reused.
On a change, a bounded log delta must contain consecutive, session-owned events
with supported types and versions before any cached prefix can be reused. The
reader fetches newest-first in 100-message API pages, back to an unchanged cached
message before all affected or unfinished messages. It compares the boundary's
projected contents, not just its ID, and refreshes the entire suffix. A watermark
check rejects a race during that read without advancing the cache.

Gapped or oversized logs, unknown event types or versions, history rewrites,
missing or regressed watermarks, and a cold cache select the full ascending
paginated read. A missing or changed boundary causes the descending read to
continue through the whole timeline. Servers that expose only a watermark across
a nonempty log range cannot safely use the incremental path.

All conversational messages and input identities are retained, not just a recent window.
Unused tool output and reasoning are discarded without changing task-part
positions. Each HTTP page and the retained projection have a 128 MiB limit.
Invalid or repeated cursors and duplicate message identities fail closed.
Pending forms and permissions are read separately on each snapshot because
they need not share that durable log. Cursor
and snapshot updates are committed only after all reads succeed. Cache state is
memory-only; restarting the bridge rebuilds it without changing delivery keys.
`opencode_service_file` takes precedence over the optional legacy `opencode_db`.
Other CLI actions, such as submitting a reply, still use `opencode_bin`.
