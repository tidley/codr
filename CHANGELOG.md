# Changelog

## [0.10.0] - 2026-10-31

This release targets existing MDK 0.14 accounts. The local combined rollout does
not verify the automated phone handover. No live handover is implied by these
notes. Fresh identity creation still uses the supported 0.9.0 setup first.

### Large OpenCode histories and chat reliability

- Fix deferred phone messages when a full OpenCode export exceeds the 128 MiB
  reader limit and blocks the synchronization checkpoint. Read ascending pages
  of 100 messages instead. Keep conversation identities and task positions, with
  bounded page/history sizes and no checkpoint advance after invalid pagination.
- Reuse cached history only when a complete supported change log proves the
  prefix is unchanged. Refresh affected suffixes and unfinished messages. Gapped
  logs, rewrites and unknown versions fall back to full pagination.
- Submit numbered and labelled native V2 decision replies through the verified
  form API. Keep request binding, legacy terminal checks and uncertain-answer
  deduplication.
- Give each new verified native session its own chat, including on a reused
  terminal. Keep old chats, accepted work and receipts with the original session.
  Saved approval holds remain explicit and uncertain creation is not retried.
- Simplify workspace creation menus: remove step counters and repeated guidance,
  shorten workspace/agent titles, and move folder-list help to the bottom. Keep
  cancellation, navigation, restart-safe selection and timeout enforcement.

### One executable with whole-application updates

- The existing same-process MDK host supports confirmed executable updates in
  one managed service. Chat upgrades keep `herdr-white-noise-bridge`; manual
  combined setup can use `codr`. Bridge and messaging code are replaced together;
  no separate `wn-agent` executable is installed by the 0.10 installer.
- `/update confirm` needs no typed code. Confirmation remains owner-only,
  chat-bound, time-limited and deduplicated. The transport timestamp must be
  newer than the offer; stale or unverifiable messages cannot confirm it.
- The updater checks the actual service, executable, configuration and messaging
  home. It restarts that same service on systemd or launchd. Combined readiness
  requires the existing local messaging account, not just a running bridge.
- Compatible MDK implementation revisions can update with the binary. A declared
  storage-cohort, protocol, bridge-state or runtime-layout change still refuses
  phone installation. Any incompatible storage change requires explicit local
  migration approval; a matching release version is not migration permission.
- Failed startup restores the previous compatible executable without rewinding
  chat or MLS account state. No uncertain installation is retried automatically.

### First handover from 0.9.0

Standard 0.9 installations can use two chat confirmations after the verified
0.10 ngit/Blossom mirror is available.
The unchanged 0.9 updater first installs the executable in external mode, with
its old displayed confirmation code. It honours its bridge-only update promise.
The new 0.10 `/update` then offers a separate private cold backup and service
handover. Confirm with `/update confirm`, without a code or terminal commands.
The handover uses the installed executable, retains the existing worker service
name for old plugin hooks, and stops the external connector. It does not create
an identity, import a fabricated release or restore old MLS state. Unknown service
overrides, nonstandard layouts and incompatible storage still require the
[manual procedure](docs/COMBINED.md).

This MVP targets existing 0.9.0 account homes. Fresh identity creation still uses
the existing local 0.9 setup before handover; a new one-binary identity-creation
wizard is not claimed by this change. Local tests do not replace cross-platform
release checks or an explicitly approved live phone update test.

### Packaging and safety

Default application packages contain one combined executable and dependency
notices. Connector protocol/upstream verification remains in CI; its separate
compatibility binary is not a default release asset. Signature, SHA-256, size,
maintainer, independent-download and rollback checks remain active.

Foreman orchestration and unrelated worker-sharing changes are not implicitly
approved by this implementation. Preserve existing accounts, keys, groups,
native sessions, history, accepted deliveries and sync settings.

### Contributors

- Tom Dwyer ([@tidley](https://github.com/tidley)): combined runtime, update
  handover, history-reader repairs, chat and prompt changes.
- Juraj Bednar: retained macOS launchd fixes and the earlier contributions whose
  original commits/co-author credits remain in the 0.9.0 ancestry. The
  [0.9.0 notes](docs/releases/0.9.0.md) retain their original GitHub/ngit contribution records.
- Marmot MDK contributors: the same pinned MDK 0.14 source and security backports
  used by the existing connector. Packages retain its MIT/MPL licenses and
  source provenance. No new MDK storage migration is part of this release.


## [0.9.0]

See the [release notes](docs/releases/0.9.0.md) and
[release verification guide](docs/releases/0.9.0-readiness.md).

### Added

- Folder menus accept case-insensitive `ls <part>` filtering, adapted from Juraj
  Bednar's [PR #17](https://github.com/tidley/codr/pull/17). Matching folders keep
  the existing 200-item pages and numbered selection. Bare `ls` clears the filter
  and refreshes page one; filtered lists survive restart without changing launch
   safety checks. Its truncation and 0.8.1 version changes are not included;
   the shared export-retry contribution is adapted separately with corrections.
- `/last` replays the newest conversation message in the current chat without
  unmuting or prompting its agent. `/last4` and `/last 4` replay the newest four
  messages in source order. Counts from 1 to 100 are supported, including text
  already delivered. Repeated transport events remain deduplicated; a new explicit
  request can replay the same text again. Other chats and delivery records stay unchanged.

### Changed

- Optional `opencode_service_file` selects non-owning V2 HTTP polling. Failed
  reads never start or replace the shared server. A durable log watermark reuses
  unchanged history; forms and permissions remain separately refreshed. Changed
  histories still use a bounded full export. The local mitigation preserved the
  shared server and connector; that deployment did not install the separate OpenCode
  recovery patch.
- `/recent` catch-up messages include the original source date and time in UTC
  alongside the You/Agent labels. OpenCode, Codex and Claude readers preserve
  message timestamps. Missing or invalid dates are marked unknown, and existing
  duplicate protection is unchanged.
- `/recent` accepts an optional lookback in hours, with or without a space:
  `/recent2` and `/recent 2` both select the past two hours. The default remains
  one hour, with at most three messages per eligible chat and existing duplicate
  protection. Invalid hour arguments receive usage guidance instead of prompting
  an agent.
- Session invitations and first chat messages identify the project and explain
  that `/help` works in any Codr chat. Invitation descriptions include this
  guidance even when automatic messages are muted.
- `/cancel` now performs a soft reset for the current chat: clear cached questions,
  permission cards, pending setup/model menus and update offers, even when the
  agent is idle or the session has ended. Interrupt the current verified active
  agent without affecting other sessions. Preserve chat history, accepted
  deliveries and records of uncertain external operations.

### Fixed

- Fresh Claude sessions read as empty until the first transcript is written,
  allowing initial chat setup to proceed. Invalid session IDs, ambiguous files
  and scan errors still fail. Adapted from Juraj Bednar's PR #18.
- CLI export fallback retries only incomplete JSON, at most three times per API
  shape. Syntax/schema errors and refused commands are not retried on that shape.
  Exhausted incomplete reads retain their diagnostic across an unsupported API
  fallback. Adapted from Juraj Bednar's shared `afe4f38` contribution; configured
  non-owning HTTP observation still cannot fall back to the managed CLI.
- Recurring bridge log lines are throttled for ten minutes with repeat counts.
  The 256-line cache evicts its oldest entry rather than hiding a new error.
  Adapted from Juraj Bednar's PR #19. Evicted entries can print again early.
- Retain the already merged PR #16 launchd bootstrap retry and slow-first-spawn
  handling in local release preparation, preserving unrelated service changes.
- The mute/wake regression waits for asynchronous checkpoint/context completion
  instead of assuming transcript reads finish within 200 ms. A deliberately slow
  fixture covers the race without changing runtime forwarding behavior.
- The bridge's optional V2 service reader cannot enter managed-server recovery
  when a history, form or permission read fails. It retries observation without
  clearing pending prompts or changing delivery identities. This addresses the
  confirmed recovery-storm mechanism, not the still-unproven initial stall.

## [0.8.0] - 2026-10-25

Phone-driven worker updates, immediate prepared-media delivery, and more reliable
project setup and progress completion. These changes are not in published 0.7.0.

### Fixed

- Attribute repeated identical phone messages one-to-one when confirmed receipt and transcript counts match. Ambiguous extra local input still mutes forwarding.
- Keep delivered blocked notices deduplicated across mute/wake. Retry cancelled undelivered notices, and do not consume an announcement while muted.
- Retry fresh-pane agent startup only after an explicit pre-input `agent_pane_busy` refusal. Other failures and uncertain submissions are not retried.
- Pause an unexpected new native session on an established terminal before creating another phone group. Retain the original chat and accepted work, persist the hold across restarts, and require local `confirm-session-change` approval against the current terminal and native session. Intentional launches in new terminals remain unchanged.
- Deliver incoming ZIP archives, documents and other file attachments as private local files, with their caption and quoted original name. Do not send them to image/audio-only validation or require a transcription service; archives are not automatically extracted or executed. Preserve bounded downloads, restart recovery and duplicate protection, and distinguish file-preparation errors from audio-transcription errors.
- Keep progress edit targets through failed completion edits, lost initial acknowledgements, restarts and mute/resume, so visible working bubbles can finish as `done` without holding up final replies. Do not complete progress on an early idle signal alone.
- Accept OpenCode's explicit `default` variant when confirming a model chosen without a variant. Previously, a successfully created session could be reported as uncertain, preventing its tab and agent from starting; in-place model changes could also be misreported. Keep exact provider/model and explicit-variant checks, with no automatic retry of earlier uncertain launches.

### Changed

- Shorten new-chat welcomes to the current chat name followed by `/help` guidance, without the obsolete bracketed agent title.
- Confirm new project setup by typing the repository folder name rather than the destination workspace label. Keep the destination visible separately, including workspace-rename revalidation and zero-cancel safety.
- Download images and transcribe voice while agents are working, then submit prepared media immediately without waiting for idle or interrupting. Keep image preparation order, existing accepted deliveries and uncertain-submission safeguards. Describe image acceptance as preparation rather than task queuing.
- Reuse the existing progress bubble for follow-ups and blocked-to-working transitions instead of appending extra `working...` notices. Preserve historical messages.

### Added

- Add `scripts/transcribe_gemini.py`, an optional voice transcriber that uses the Gemini API. It sends the voice audio to Google. The API key is read from `~/.config/codr/gemini-api-key`. Local Phonon remains the default.
- Answer supported OpenCode V2 questions with custom text directly from the phone, such as a version number, or `/answer TEXT`. Bind the reply to the fresh native form and session, retain question-code and duplicate guards, and never turn it into a queued prompt or arbitrary terminal input. Legacy, multi-select and multi-question custom replies remain terminal-only.
- Owner-only `/update` discovers stable releases from the pinned ngit maintainer, asks for a chat-bound confirmation, and verifies independent Blossom downloads before replacing only the managed bridge worker. Updates retain state and accepted work, reject connector/storage-cohort changes, and restore the previous executable after failed startup without rewinding routing state.
- Optional standalone control chat (`control_chat = true`): one owner-only White Noise group that accepts `/new` and general commands without being mapped to an agent session. Plain text, session-only commands, and uncertain creation are handled explicitly.
- Commands accept unambiguous prefixes (`/n`, `/st`, `/sy`); in the control chat the leading `/` is optional (`new`, `n`, `status`). Ambiguous prefixes are listed in the control chat and left to the agent elsewhere.
- The `/new` folder step accepts `ls` to list selectable directories under `launch_root` and choose one by number.
- A pending launch starts on `confirm`, `yes` or `y`. Typing the repository folder name still works; the workspace label is not the confirmation name.

## [0.7.0] - 2026-10-25

### Added

- Bare `/new` now first offers an existing **herdr** workspace or a new one. Existing destinations receive a new project tab without changing their current sessions, chats, queues, or workspace labels. Workspace menus are paginated and restart-safe; final confirmation includes the destination label and stable ID. Removed or unavailable workspaces stop launch, while renamed workspaces require a new confirmation. No fallback workspace is created automatically.
- Guided `/new` project setup asks for a folder, installed agent, and model before confirmation. Chat-bound menus survive restarts, offer a default or provider categories, and list each provider's models and variants in full. Existing chats gain `/agent` for a separate session and `/model` for confirmed, idle-only OpenCode V2 model changes. Codex/Claude model choices start separate sessions without moving old history or deliveries. Guided OpenCode launches create the selected native session before attaching the terminal.
- Owner-only `/new NAME FOLDER AGENT` previews a workspace launch under `launch_root` (default `~/code`). Both existing and missing folders require a chat-bound confirmation before creating folders or starting OpenCode, Codex, or Claude Code. Requests survive restarts; uncertain startup is never retried automatically.
- Owner image messages retain captions in a persistent per-chat media FIFO. Each image message occupies one of 16 waiting slots. Images are prepared when their item reaches an idle agent, and private prepared paths survive restarts. `/queue` includes these pending deliveries. Voice handling is unchanged.
- Send explicitly embedded local Markdown images in new completed agent replies as encrypted White Noise attachments. Source files must be regular, non-symlink PNG, JPEG, WebP, or GIF files beneath the mapped working directory or `/tmp/opencode`. Private snapshots and stable delivery IDs preserve retry bytes across restarts. Ordinary links stay text; unavailable or blocked images get a visible notice. Limits are four images, 10 MiB per image, and 25 MiB per reply. The connector must allow the private outbound-media spool; fresh setup configures it.
- Owner-only `/cancel` interrupts the current task through herdr without deleting history, stopping workers, or clearing accepted deliveries. Native session identity is checked immediately before submission, and uncertain interrupts are never automatically retried.
- Persistent `/sync auto|on|off` modes, with the current mode displayed in help. Forced modes survive restarts and ignore automatic mute/wake changes; `/unmute` clears the override and restores automatic sync.
- Owner-only OpenCode permission replies with `/allow CODE` and `/reject CODE`. Codes are bound to the pending request and checked against the live session before submission.
- An opt-in, same-user local endpoint for the separate **portr** app to list sessions, read bounded recent history and submit persistent desktop deliveries. Request IDs prevent duplicate enqueueing. SSH can carry requests without exporting the bridge's Nostr identity; codr does not install SSH, a VPN, or the desktop app.

### Changed

- Phone text is handed directly to the mapped native agent while it is working, without interrupting or waiting for idle. Existing accepted deliveries remain intact. Multiple follow-ups preserve response tracking and native-input attribution across staggered transcript updates and restarts.
- Rename catch-up `/sync` to `/recent`, with the help description `Get newest 3 of all active <1h`. Its activity window is now the hour before the request, not an hour before the last global confirmed send. Existing source-message deduplication remains intact.
- Retire `/clear-queue` from supported commands and help. Old standalone invocations return a retirement notice without deleting accepted work. `/queue` remains a pending-delivery inspection command.
- Model menus now offer default/provider selection first and show a provider's full available model list without pagination. Variants are also listed in full; long lists use the existing durable message splitting. Help uses bold section headings with each command's description on the next line, matching the updated phone layout.
- Use bracketed four-step setup headings and agent names **OpenCode**, **Codex**, and **Claude Code**. Group large model catalogues by provider/free pricing, then list base models and variants separately instead of expanding every variant into one long list. Confirm a launch by typing the displayed workspace name within ten minutes; zero-cancel and explicit command compatibility remain available.
- Tidy project menus place numbered **Create new workspace** after existing workspaces, accept plain folder names and offer **0. Cancel** without clearing uncertain operations. Special choices render separately from ordered Markdown lists, so WhiteNoise cannot renumber cancellation or paginated workspace creation.
- Project setup menus accept plain numbered replies, with no long code to copy. Active menu choices are routed before agent decisions. Folder commands and explicit launch/model confirmations also work without a code; existing coded commands, expiry, sender checks, and final validation remain supported.
- Fresh setup gives newly created bridge accounts the public display name **Pat**. New session chats introduce Pat and point to `/help`. Existing identities, custom profiles, and chats remain unchanged; unconfirmed profile publication is reported without automatic retries.
- Make the **portr** desktop/SSH endpoint opt-in instead of starting it with every worker. Fresh setup defaults to phone-only access and offers an optional desktop choice; non-interactive setup uses `--portr`. Existing explicit preferences are preserved, and a changed preference is saved for an idle worker restart rather than triggering an extra restart. `run --portr` and `run --no-portr` override the configuration; earlier `--porter` spellings remain aliases. The desktop app stays separate, and codr does not install SSH or VPN services.
- `/help` uses Markdown headings, bold inline-code commands and descriptions on the next line. `/status` explicitly describes unmuting, and help shows the current persistent sync mode. Catch-up excerpts have section breaks between their existing You/Agent labels.
- Fresh sessions launched from an existing chat keep the source tab label when available and distinguish their new chat with an agent suffix inside the workspace parentheses, for example `review (Acme - codex)`. The original chat is unchanged. Suffixes survive restarts and renames and are preserved within the title byte limit.
- Show a fresh `> working...` notice when an agent resumes from a blocked decision or a phone follow-up is delivered while it is working. Pending notices coalesce; polling and restarts do not repeat them. Refreshes are best effort and follow durable answers.
- White Noise chat names now match the **herdr** tab and workspace as `review (Acme)`, without the `[cd]` prefix. Existing managed chats use the same format after a profile update or `rename-groups`.
- Retained media acknowledgements say only `Queued prompt at position N.`, including position 1; immediate text no longer receives a queue acknowledgement.
- `/recent` skips unchanged source messages already confirmed sent or pending in each chat, including previous catch-ups and ordinary replies. Per-source delivery records survive restarts and confirm only after every message chunk succeeds. Excerpts preserve conversation order, can include older context or unanswered user prompts, and exclude tool output and unfinished replies.

### Fixed

- Clear expired project menus after one expiry notice so later phone messages return to normal agent routing. Preserve uncertain actions and retry safely after a failed state save.
- Keep control replies after owner inputs across whole-second timestamp ties and small clock differences. Persist the per-chat send deadline across restarts. Do not delay native agent input or replies in other chats.
- Keep stable executable paths across Homebrew upgrades and report missing OpenCode executables without exposing transcript data.
- Preserve optional OpenCode support for Claude-only and Codex-only installations, including repeat setup and check mode. Bare commands retain PATH lookup.
- Validate replacement session readers before saving an OpenCode repair. Restore the old configuration after a failed worker restart so setup can retry the repair.
- Read Claude transcripts larger than 128 MiB and parse only appended bytes, retaining the bounded 32-entry transcript cache and eviction order.
- OpenCode V2 decision replies no longer reject a matching live question when the sidebar shows a repository-path footer instead of the older version footer. The bridge still checks the current request identity, exact question and options, visible selection controls, and unchanged terminal session before sending a choice.

### Contributors

- **Paul Eipper ([@lkraider](https://github.com/lkraider))** — Claude transcripts above 128 MiB, incremental parsing, bounded-cache and eviction tests, and load-tolerant bridge test waits. These contributions came through ngit; original commit authorship is preserved.
- **Juraj Bednar ([@jooray](https://github.com/jooray))** — stable OpenCode executable paths across Homebrew upgrades, via [GitHub PR #6](https://github.com/tidley/codr/pull/6).
- **Tom Dwyer ([@tidley](https://github.com/tidley))** — phone controls, project/model setup, media delivery, integration, follow-up fixes, documentation and release work.

## [0.6.2] - 2026-10-23 (Linux preview)

### Added

- Owner-only `/sync` sends the latest completed assistant reply from conversations active since one hour before the last confirmed message across all linked White Noise chats, each to its own chat. The global reference persists across restarts and is recovered from MDK history on older installations; no-history installs fall back to the past hour. Recently ended mappings remain eligible, and mute settings and normal reply checkpoints stay unchanged.
- Owner-only `/help` lists White Noise commands and decision-reply syntax in the requesting chat. Both commands work while muted, never prompt agents, and retain durable replay-safe replies across bridge restarts.

### Changed

- Build the dedicated connector from MDK 0.14.0 (`3fb4b4759ac676b0c21c4c3527074866c9fa4c89`), retaining the founding-admin and media-recovery extensions. Linux previews rebuild this connector instead of reusing 0.13 assets.
- MDK 0.14.0 rekeys account databases and migrates storage on first open. Keep a device-local backup of the complete connector home and secret-store credentials before upgrading; downgrade to 0.13 is unsupported.

### Fixed

- Muting suppresses chat messages, not group invitations or automatic chat-name and description updates. Metadata updates do not wait for transcript reads.
- Introduction messages use bracketed lowercase agent labels, such as `[oc] Casual greeting`, instead of legacy labels such as `OC | Casual greeting`.

## [0.6.1] - 2026-10-23 (Linux preview)

### Added

- Send `unmute` or `/unmute` in any linked White Noise chat to resume forwarding for all chats without sending agent input. Owner checks, history checkpoints, and duplicate-message protection still apply.
- Opt-in Linux-only prerelease builds for x86-64 and ARM64, with verified reuse of the unchanged dedicated connector. Stable macOS releases remain separate.

### Fixed

- Recover pending OpenCode V2 question cards from live session forms. V2 transcript exports omit running questions. Existing session, choice, and visible-terminal checks still apply before an answer is sent.
- Keep the working checklist visible while a final reply is being saved, send completed replies separately, and preserve checklists from both working-label formats.

## [0.6.0] - 2026-10-22

### Added

- Automatically mute all groups on local native prompts and resume on authorized phone input. Preserve manual controls without opening a startup popup.
- macOS support for the Rust bridge, the MDK transport, and the local Phonon-2 worker.
- launchd LaunchAgents on macOS, with the same restart-on-failure behaviour as the systemd units.
- Native macOS aarch64 and x86_64 release binaries, ad-hoc signed, alongside the existing musl builds.
- Setup finds a compatible interpreter for Phonon, including Python 3.10–3.12 on Intel macOS.
- Phonon installs its MLX runtime on Apple silicon and the CPU runtime on Intel Macs.
- New groups grant admin rights to the configured phone owner and bridge in the founding state, including the initial Welcome.
- Release-matched connector binaries, built from a pinned MDK revision and the published founding-admin/media-recovery patch.
- Managed connector upgrades that preserve account state, group mappings, and transport arguments. Custom overrides require inspection.

### Changed

- Service supervision moved behind one interface shared by systemd and launchd.
- The install hook and setup tolerate macOS's Python 3.9 when only stdlib parsing is needed.
- Group names include the workspace, with UTF-8 byte limits.
- OpenCode session exports support both 1.x and 2.x formats.
- Live group titles follow **herdr** label changes, with durable rename intent and protection for manually renamed groups.
- Session readers select CLI export when an OpenCode database history format becomes incompatible, including upgrades while the worker runs. Storage errors still defer reads.
- Setup checks live transcript readers before changing services or identities. `check-readers` repeats that read-only validation without printing transcripts.

### Known limits

- Automatic handoff observes native transcripts after polling and read latency. It cannot recall output already submitted to MDK. Set `auto_forwarding = false` to retain manual local-activity control.
- Phone prompts are not queued behind busy or blocked agents. Wait for idle and resend; supported decision answers remain available while blocked.
- launchd has no memory cap, so the 1 GiB Phonon limit applies only on Linux.
- macOS setup needs a logged-in launchd session; a headless SSH session cannot bootstrap the agents.

### Compatibility

- Upgrade the bridge and dedicated connector together through setup. Shared `wn-agent` installations are not replaced.
- Existing groups retain their current admins. Group names do not affect routing, and additional admins do not receive agent-prompt permission.
- On Linux, the dedicated connector requires glibc 2.35 or newer; its OpenSSL dependency is bundled.

## [0.5.0] - 2026-10-22

### Added

- Image attachments with captions and private local files for the mapped agent.
- Local Phonon-2 voice transcription, submitted directly as an agent prompt.
- A quoted transcript beneath `transcribed`, edited into the original `transcribing` bubble when MDK supports edits.
- Optional incremental, read-only OpenCode SQLite reads through `opencode_db`. Unchanged sessions fetch no message or part bodies.
- Background transcript reads, with up to four readers and one outstanding read per session.

### Changed

- Chat names use `[cd] session`. Descriptions contain workspace and agent details.
- Status notices use blockquotes. Task lists appear below the status without a `Tasks` heading.
- README screenshots cover images, voice, decisions, and shared chats with owner-only agent control.

### Fixed

- Pending and prepared media survive restarts and temporary **herdr** outages without duplicate prompt submission.
- Deferred media in one session does not block preparation for other chats.
- Transcript reads preserve session identity and forwarding-mode checks. Exiting sessions get a fresh final read.
- Transcription notice delivery retries independently of agent submission, including lost send or edit responses.

### Compatibility

- Same-bubble transcripts and editable progress require `edit_responses = true` and an MDK build with `edit_message` support.
- Offline media recovery needs MDK's opt-in `include_media` download references. These extensions are not in published MDK 0.13.0 binaries.
- Incremental database reads were verified against OpenCode 1.19.3's v1 projections and durable event log. The CLI reader remains available when `opencode_db` is unset.

[0.6.0]: https://github.com/tidley/codr/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/tidley/codr/compare/v0.4.0...v0.5.0
