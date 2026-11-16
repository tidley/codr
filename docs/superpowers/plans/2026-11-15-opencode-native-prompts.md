# OpenCode native prompt delivery

## Approved scope

Use the existing OpenCode V2 service for ordinary phone prompts, including prepared
media text. Keep Codex, Claude, and legacy OpenCode without a configured V2 service
on the existing herdr terminal path. Do not change decisions or permissions.

## Delivery contract

- Persist a unique `msg_codr_…` ID in the existing submission receipt before POST.
- POST once to the exact mapped session, preserving its agent and model.
- Validate the returned native inbox receipt against session, ID and text.
- After an uncertain result, reconcile by ID from saved messages or the inbox.
  Never retry the POST or switch to terminal input.
- Reconcile retained IDs after restart. Older terminal receipts keep their existing
  fingerprint/baseline rules. Preserve accounts, histories, drafts and receipts.
- Use the existing authenticated loopback service registration, bounded responses,
  no redirects or proxies, and no automatic service startup.

## Execution

1. Add failing acceptance-ID and HTTP delivery regressions.
2. Add a small prompt transport module; share service registration validation.
3. Route only configured OpenCode V2 prompts through it; persist IDs and reconcile
   uncertain native admissions during normal mapping reconciliation.
4. Run focused and full host tests, formatting and strict Clippy. Request two
   independent read-only reviews and resolve demonstrated findings.
5. Build once, record exact source/artifact hashes, cold-back up bridge state,
   replace only the local executable, and verify messaging/native bindings.
   Keep MDK 0.17 storage untouched. Do not commit, push or release.
