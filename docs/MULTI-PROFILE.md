# Additional app profiles and existing chats

Issue #33 is superseded by [#34 shared groups with named agents](SHARED_CHATS.md).
The single-session `attach-chat` compatibility path below remains available; it
does not enable shared `@alias` routing or authorise Third Ear runtime repairs.

This source feature adds a local `attach-chat` operation. It does not replace
`phone_npub`, create an account, create/invite a chat, start an agent, migrate MDK
storage, or install/restart anything. The existing phone remains the owner.

## Local operator setup (separate runtime approval required)

1. Verify the full additional app `npub` and existing chat's lowercase hex group
   ID using your trusted local messaging tools. Do not identify either by name.
   The bridge's existing messaging account must already belong to the chat.
   Verify the intended app is a member yourself: the connector's `group_info`
   exposes account/group IDs and member count, not a member public-key roster.
   Membership never populates Codr's controller allowlist.
2. Stop **only Codr's worker**, under the applicable service approval. Preserve
   its existing config, state, accounts, MDK storage and backup gates. The command
   takes the normal exclusive worker-state lock and refuses a running worker.
3. Select a dedicated idle native session in Herdr, with a supported transcript
   reader (OpenCode, Codex or Claude). Provisioning it is a separate native action,
   not something `attach-chat` does. It must have no retained Codr mapping, and
   its terminal must not belong to another retained mapping. Do not delete old
   mappings to make a reused terminal pass validation. Attach before letting the
   worker discover the dedicated session and automatically create another chat.
4. Keep Herdr available. If using an external connector, keep the compatible
   **same MDK cohort** connector available at the configured socket. For a combined
   installation, stopping Codr also stops its embedded connector. After the
   separately approved stopped-service/backup gates, add
   `--messaging-home /absolute/path/to/existing/account-home` to the command below.
   This temporarily hosts this binary's same-cohort connector and shuts it down
   after attachment or failure. It does not create an account. Normal connector
   relay/background activity still applies while it runs. Never start it while
   another process owns the socket or reads that MDK home. A missing external
   socket or failed embedded startup fails safely; startup waits at most ten
   seconds without retrying the attachment.
5. Obtain the session key from Herdr's native session metadata. It is the exact
   JSON array `[source, agent, kind, value]`, not a pane ID, title, model/profile
   label or guessed session name. Run with the existing configuration:

   ```sh
   herdr-white-noise-bridge --config /absolute/path/config.toml attach-chat \
     '["herdr:opencode","opencode","id","ses_DEDICATED"]' \
     --group GROUP_ID_HEX --profile npub1ADDITIONAL_APP
   ```

   The executable name above is the Cargo binary; packaged installations may use
   their existing `worker` executable instead. Placeholders are not real IDs.

6. Inspect `status` with that config to confirm the session/group binding. Resume
   the worker only under the existing service approval, and send a **new** app
   message after the attachment second has passed. Its response stays in that
   chat. No old test audio needs resending or replaying automatically.

Attachment validates the live session, group/account response and full paged
timeline, reads the existing native transcript, then rechecks the native identity
and idle sequence before saving one mapping. Existing mappings, names, receipts,
phone binding and account are retained. The attached chat keeps its existing name
locked against automatic renaming. Attachment itself only issues read requests
to Herdr and the connector; optional connector startup is the existing runtime
path, with the same storage and service safety boundaries as `run`.

All observed chat message IDs and existing final-reply IDs become durable
baselines. Subsequent attached-chat input must carry a `recorded_at` second
strictly later than attachment; missing, old and same-second timestamps are
ignored, including after restart and timeline recovery. This intentionally drops
messages arriving during setup. Attachment cannot be used to rebind either an
existing mapping or an already-mapped chat.

## Authority and media

- The additional key controls only its explicitly attached chat, including
  ordinary prompts and native numeric/form/permission answers in that session.
  Other members, other app keys, self events, other bridge accounts and unrelated
  groups cannot use that authorisation. The original phone retains owner access.
- Additional profiles cannot use slash bridge commands (including abbreviations
  and serial commands), inspect other chats, launch sessions, install updates or
  change global forwarding. Bare replies cannot answer the owner's setup or model
  menus either. While forwarding is paused, the app gets a chat-local
  notice instead of waking all phone chats or submitting a prompt. Ask the owner
  to resume forwarding. Bare `unmute` from an app is ordinary agent text, not a
  forwarding control.
- New media uses the existing bounded download/transcription path and native
  acceptance safeguards. Configure the existing `transcribe_command` for audio;
  unavailable transcription/media produces the existing preparation-failure
  notice, with no agent prompt. An uncertain native submission is not retried.
  Authorisation is agent access, not an OS sandbox: use a suitably restricted
  dedicated agent/workspace and retain its normal native permission gates.

## Local fixture verification

The tests in `tests/bridge.rs` use temporary private Unix socket peers, synthetic
public keys, shell transcript fixtures and a fixed-output transcription fixture.
They do not open an MDK account, contact relays, create discoverable native
sessions/panes, send live messages or call a model. No live setup above is needed.

Run these commands from the repository root with the normal Rust dependencies and
configured build-storage adapter available. Tests also require the repository's
[pinned Odin compiler](COMBINED.md) for MDK Markdown; managed workers leave its
provisioning to their supervisor. No account or runtime configuration is needed:

```sh
cargo test --locked --test bridge attached_profile_
cargo test --locked --test bridge attachment_validation_
cargo test --locked --test bridge app_commands_
cargo test --locked --test bridge profile_allowlists_
cargo test --locked --test bridge additional_profile_audio_
cargo test --locked --test bridge additional_profile_without_transcription_
cargo test --locked --test bridge legacy_phone_without_profile_fields_
cargo test --locked --test bridge native_opencode_prompt_persists_
```

These focused fixtures cover paged attachment baselines, profile/chat/account
rejection, old audio suppression, restart recovery of new text/audio, response
destination, command isolation, validation failures, retained state and legacy
phone access. The configured full Rust tests, format, Clippy and Python checks,
plus two independent exact-candidate reviews, remain the qualification gates.
Fixture success is not live installation/runtime verification or review approval.
