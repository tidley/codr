# **codr**

**codr** connects coding agent sessions in [**herdr**](https://github.com/herdrdev/herdr) to [White Noise](https://github.com/marmot-protocol/whitenoise-android), with one encrypted chat per session. Send a message from your phone to continue coding, create a project, choose an agent or model, and read the replies in the same chat.

Supported agents: OpenCode, Codex, and Claude Code on Linux and macOS.

New bridge accounts introduce themselves as **Pat**, your messenger between White Noise and herdr. Verify the full bridge public key printed by setup, not its display name.

## Behaviour

- Chat names pair the **herdr** tab and workspace: `review (Acme)`. Descriptions contain workspace and agent details.
- With editable progress enabled, one active bubble carries status and checklist updates. Final replies stay separate.
- Phone text is handed straight to the agent, including while it is working. Images, files and voice follow as soon as download/transcription completes, without waiting for idle. The agent handles its own input; codr does not interrupt it. Send `/cancel` when you do want to interrupt the current task.
- `/new` guides you through workspace, folder, installed agent, and model selection. Reply with numbers, use `0` to cancel, and type the repository folder name to confirm the final preview. The destination workspace is shown separately.
- Choose the default model or browse a provider's full model list. OpenCode uses its authenticated model catalogue; variants are selected separately. `/agent` opens a separate chat without moving old history.
- Supported OpenCode questions accept a number or option label as a reply. V2 questions with custom input also accept text answers, such as a version number.
- New groups make your configured account an admin, so you can invite participants and rename the group. Only the owner can prompt the agent. [Group setup](docs/REFERENCE.md#shared-chats-and-owner-only-control)
- In automatic sync mode, local prompts mute all chats and an owner phone message resumes them. `/sync on` or `/sync off` keeps your explicit choice across restarts; `/unmute` restores automatic mode.

## Install

**0.11.0 targets existing accounts.** Fresh accounts use the
supported 0.9.0 setup below, then the owner-confirmed chat handover. See the
[0.11.0 release notes](docs/releases/0.11.0.md) and
[combined runtime/handover guide](docs/COMBINED.md).

Needs Linux x86_64 or aarch64 or macOS on Apple silicon or Intel, a per-user service manager (systemd or launchd), Python 3.11+, Git, and **herdr** 0.9.1+. No Rust toolchain required.

```sh
herdr plugin install tidley/codr --ref v0.9.0
herdr plugin action invoke local.white-noise.setup
```

Enter **your White Noise public key** (`npub`) during setup. Get White Noise for [iOS](https://github.com/marmot-protocol/whitenoise-ios) or [Android](https://github.com/marmot-protocol/whitenoise-android).

Or give your coding agent this prompt, with your own public key:

> Install https://github.com/tidley/codr. My White Noise public key is npub1…. Follow docs/INSTALL.md and report what you verified.

[Full installation and upgrade guide →](docs/INSTALL.md)

### New in 0.11.0

- Completed Claude replies reach the phone while background tasks keep the
  session working. System task notifications do not count as local typing or
  mute forwarding, and completed same-turn replies remain available.
- Progress keeps its existing bubble until the session settles, without leaving
  an unfinished replacement after a completed background reply.
- A no-update check says **No new version available. Currently running codr
  0.11.0.** It does not claim that an installation occurred.
- Integrates Juraj Bednar's [PR #20](https://github.com/tidley/codr/pull/20).
  Corrected adaptations of PRs #17–19 remain from 0.9.0.

### Retained from 0.10.0

- One executable hosts bridge and messaging in one managed service. Chat upgrades
  keep the existing worker service name.
- `/update confirm` needs no code and updates both embedded components together.
- Existing 0.9 users have a two-stage chat upgrade: first the bridge-only update,
  then a separately confirmed cold backup and combined-service handover.
  The first step still uses 0.9's old confirmation code. Supported managed
  installations need no terminal commands. [Upgrade details](docs/COMBINED.md).
  This implementation targets existing accounts.
- Large OpenCode histories use bounded 100-message pages instead of full exports,
  so the 128 MiB reader limit no longer blocks synchronization of the whole export.
- New verified native sessions get separate chats, including on reused terminals.
  Native V2 decision choices use their verified form API, and project menus are shorter.

### Retained from 0.9.0

- Read this chat's latest message with `/last`, or several with `/last4`, without
  resuming sync. `/recent [hours]` keeps its three-message excerpt per active chat.
- Search existing project folders with `ls <part>` in `/new`. Results keep
  200-folder pages, `next`/`previous` navigation and restart-safe selection.
  Bare `ls` clears the search and refreshes the full list.
- Use an optional non-owning OpenCode HTTP reader: failed reads cannot start or
  replace the shared server. CLI fallback retries only incomplete JSON.
- Start fresh Claude chats before a transcript exists, and reduce repeated
  warnings without hiding first-occurrence errors when the logging cache fills.
- Keep history and accepted deliveries when `/cancel` clears stale chat controls.
  History excerpts show original UTC source dates rather than the request time.

Juraj Bednar's folder filtering, fresh-Claude-session and logging contributions
are adapted with fixes and regression tests. His approved macOS launchd fix is
retained. See the release notes for contributor credits and scope.

## Usage

**Upgrading from 0.6.0 or an older MDK 0.13 installation?** 0.7.0 uses MDK 0.14. Before first opening that storage with the new connector, stop the services and keep a private, device-local backup of the complete connector home and secret-store credentials. The migration is one-way; downgrading to MDK 0.13 is unsupported. Setup refuses an automatic cross-cohort upgrade. Follow the [upgrade guide](docs/INSTALL.md) and [0.7.0 notes](docs/releases/0.7.0.md); do not bypass the backup gate. Existing MDK 0.14 installations retain their account and chat history.

1. Check the inviter against the bridge public key printed by setup, then accept the session's chat invitation.
2. Send `/status` to check the connection. Send a prompt to continue that session.
3. Read the reply in the same chat. For a supported decision, reply `1`, `2`, or an exact option label.

| Command | What it does |
| --- | --- |
| `/help` | Show commands and the current sync mode. |
| `/update` | Check stable ngit releases. Confirm an application update or the separate 0.9-to-combined service handover. |
| `/status` | Show connection, state and pending deliveries; resume sync in automatic mode. |
| `/recent [hours]` | Get the newest three text messages from each chat active in the past hour, or the specified number of hours. `/recent2` and `/recent 2` are equivalent. Skip messages already delivered or pending. |
| `/last [count]` | Replay the newest message in this chat without unmuting. `/last4` and `/last 4` replay four messages, including already delivered text. Count: 1–100. |
| `/new` | Set up a project, with a final name confirmation. |
| `/agent` | Choose an agent in a separate session and chat. |
| `/model` | Choose a model; OpenCode supports a confirmed idle-session change in place. |
| `/cancel` | Clear pending questions and menus in this chat and interrupt its current task, keeping history and accepted deliveries. |
| `/queue` | Inspect pending media and deliveries accepted before the update. |
| `/unmute` | Resume all chats and clear a forced sync mode. |
| `/sync auto\|on\|off` | Use automatic sync, always on, or always off. The choice persists across restarts. |

In 0.7.0, **`/recent` replaces the old catch-up `/sync` command**. It sends each eligible chat's excerpt to that chat, with **You** and **Agent** labels. Help and history retrieval do not prompt agents or change sync mode. `/last` is an explicit replay for the current agent chat only; it does not use `/recent`'s activity window or skip already delivered text.

Catch-up messages include each source message's date and time in UTC under its
sender label. Missing or invalid source timestamps show **Date unknown**, not the
time of the catch-up request. Adding dates does not reset duplicate protection.

`/cancel` is a chat-scoped soft reset, including when the agent is idle. It clears
cached questions, permission cards, pending setup/model menus and update offers.
It does not restart services, stop other sessions, change the selected model, or
discard accepted deliveries. Records of uncertain external operations remain for
reconciliation. An interrupt acknowledgement does not prove the agent has stopped.

New phone text does not join a worker-scheduled task queue. Messages are delivered without an interrupt; a backend can still use its own native inbox. A blocked permission or decision requires its supported answer or `/cancel`, not an arbitrary prompt. Media still needs download/transcription, and existing accepted deliveries are preserved across upgrades. `/clear-queue` is retired and does not discard those deliveries. A lost submission acknowledgement is never automatically retried.

Handoff is automatic. Manual controls remain available through **herdr**'s **White Noise forwarding: At desk / Away** action. [Controls and limits →](docs/REFERENCE.md#at-desk--away-forwarding)

**Phone updates:** `/update` shows an offer; reply **`/update confirm`** within ten minutes in 0.10, with no code. `/update cancel` dismisses it. The first 0.9-to-0.10 update keeps the external connector and still uses 0.9's displayed code. Send `/update` again in 0.10 to confirm the private cold backup and combined-service handover. Supported standard installations need no shell commands. Later combined updates replace bridge and messaging together. Unsupported layouts and storage migrations still require local setup. [Upgrade details →](docs/COMBINED.md) · [Update safety →](docs/REFERENCE.md#confirmed-updates-from-ngit)

To test an update, wait for the stable release and its verified ngit/Blossom
mirror, then send `/update` from the owner's phone. Check the offered version
before using the confirmation command. A CI artifact or GitHub-only release is
not discoverable by this command. Cancel if the version is not the one you intend
to test; do not bypass the signature, checksum or connector-cohort checks.

### Control chat

Session chats are tied to native agent sessions. Their history remains after a session ends, and restoring that session reconnects its existing chat. Set `control_chat = true` in the bridge config and restart the worker to add one owner-only chat called `codr control`. The bridge invites your phone on the next start and keeps the chat when sessions are renamed or end.

Send `/new` there to start a project, or use `/help`, `/status`, `/recent`, `/queue`, `/sync`, and `/unmute`. `/agent`, `/model`, and `/cancel` need a session chat. Plain text gets a short hint instead of reaching an agent. This is also where `/new` works on a fresh install before any session chat exists. [Control chat reference →](docs/REFERENCE.md#control-chat)

## How it works

White Noise ↔ MDK ↔ **codr** ↔ **herdr** ↔ your agent

[MDK](https://github.com/marmot-protocol/mdk) handles identity, encrypted messaging, and relays. **codr** routes prompts through **herdr** and reads completed replies from existing agent transcripts.

Reply collection uses existing transcripts without an additional model call or terminal scraping. [Behaviour and recovery](docs/REFERENCE.md)

OpenCode can use incremental, read-only SQLite reads instead of full exports. [Reader configuration](docs/REFERENCE.md#incremental-opencode-database-reads)

**Limits:** Decision replies support OpenCode single-choice questions with 1–9 options. Other dialogs need the terminal. Avoid simultaneous terminal/phone answers. [Decision compatibility](docs/REFERENCE.md#answer-decisions-from-the-phone) · [Media and audio](docs/REFERENCE.md#images-and-voice-messages)

**Agent screenshots:** A new completed reply containing `![Screenshot](path/to/screenshot.png)` sends the image as an encrypted White Noise attachment. Files must be inside that session's working directory or `/tmp/opencode`. Ordinary links are not uploaded. Existing installations need the connector's private outbound-media spool enabled; see [outbound images](docs/REFERENCE.md#images-from-the-agent).

## Roadmap

[Planned work and completed items](docs/REFERENCE.md#roadmap).

## Features in action

Explore the fictional Garden planner project through real **herdr** and White Noise interfaces. Phone close-ups show real command replies in the stock Android app; click them for the full app view. Side-by-side demos use scripted agent replies, encrypted messages, and local speech-to-text. [Capture details](docs/REFERENCE.md#screenshot-provenance).

### Start a project

Send **`/new`** to choose an existing workspace or create one. Nothing starts before the final confirmation.

<a href="docs/screenshots/codr-v070-android-new-full.png"><img src="docs/screenshots/codr-v070-android-new.png" width="520" alt="The phone sends /new, then codr replies with workspace choices and 0. Cancel"></a>

### Choose a model

Send **`/model`** in an idle OpenCode chat to browse providers. The current model stays unchanged until confirmation.

<a href="docs/screenshots/codr-v070-android-model-full.png"><img src="docs/screenshots/codr-v070-android-model.png" width="520" alt="The phone sends /model, then codr replies with OpenAI, Claude, and Free provider choices"></a>

### Follow progress

Read working status and task progress in the session chat. Editable checklists require `edit_responses = true`.

<img src="docs/screenshots/herdr-phone-progress.png" width="1280" alt="herdr with the Garden planner task checklist beside matching working status and editable tasks in White Noise">

### Share a project

Alex invited Sam to the Garden planner chat. Sam can discuss the project, but only Alex's messages reach the agent.

<img src="docs/screenshots/herdr-phone-shared.png" width="1280" alt="Alex, Sam, and codr in a three-member White Noise chat, with only Alex's request shown in herdr">

### Send an image

Attach a design from your phone so the agent can review the downloaded image.

<img src="docs/screenshots/herdr-phone-image.png" width="1280" alt="A watering-reminder design uploaded in White Noise beside its downloaded local image path and scripted review in OpenCode">

### Send a voice message

Record a message on your phone; local speech-to-text turns it into an agent prompt.

<img src="docs/screenshots/herdr-phone-voice.png" width="1280" alt="A White Noise audio message and transcribing status beside the actual Phonon-2 transcript sent to the demo agent">

### Answer a decision

Reply with a number to answer a supported OpenCode question without returning to the terminal.

<img src="docs/screenshots/herdr-phone-decisions.png" width="1280" alt="A ten-minute snooze decision in OpenCode beside the White Noise question card, phone reply 1, and choice confirmation">

### Read a review

Read the agent's review in the same chat, alongside the matching herdr session.

<img src="docs/screenshots/herdr-phone-review.png" width="1280" alt="The herdr review tab beside its White Noise chat, both showing checks for the Garden planner watering reminder">

### Keep sessions separate

Each herdr session has its own chat. To collect them in one place, open White Noise's Folders settings, create an intelligent folder, and filter it by the bridge account (the full public key is printed during setup). New codr chats join the folder automatically, and renaming a chat does not move it out.

<img src="docs/screenshots/herdr-phone-conversations.png" width="1280" alt="Six Garden planner session tabs in herdr beside their six matching chats in a White Noise Garden folder">

---

[Operating reference](docs/REFERENCE.md) · [Changelog](CHANGELOG.md) · [Releases](https://github.com/tidley/codr/releases)

Built on [**herdr**](https://github.com/herdrdev/herdr), [White Noise](https://github.com/marmot-protocol/whitenoise-android), [MDK](https://github.com/marmot-protocol/mdk), and the [Marmot protocol](https://github.com/marmot-protocol/marmot). [Full credits](docs/REFERENCE.md#acknowledgements).
