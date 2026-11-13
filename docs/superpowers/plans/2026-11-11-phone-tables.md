# Readable phone tables

**Goal:** Convert wide outgoing Markdown tables into labelled rows in Codr only.

Use the existing `pulldown-cmark` parser and source offsets. Convert top-level
tables with at least four columns or visible column widths above 80 display cells.
Keep small tables, nested tables, code blocks and surrounding text unchanged.
Each row starts with its first header/value as a heading. Remaining fields use
labelled bullets. Keep original inline Markdown, link targets and reference
definitions. Reference links become self-contained inline links in converted
messages, so chunking cannot separate targets from definitions. Empty header
labels use `Column N`.

Apply conversion when text enters the durable outbox, before UTF-8 transport
chunking. Automatic replies and manual history then use the same display rule.
Do not change source message identities, history, receipt keys or existing queued
payloads. No dependency, version, Android, deployment or publication change.

1. Add production-outbox regressions for the screenshot's five-column shape,
   small/code/nested preservation, links/inline syntax, escaped pipes, long
   UTF-8 payloads and restart/idempotency behaviour. Observe missing conversion.
2. Add private `src/phone_markdown.rs` and call it from `enqueue_text` in
   `src/state.rs`. Return borrowed text when no table changes. Restrict parsing
   to messages containing `|` and keep the transformation idempotent.
3. Update the reference and unreleased changelog. Run the focused tests, strict
   Clippy, formatting, relevant bridge/history suites and Python contracts.
4. Get two independent read-only reviews, address demonstrated findings and
    verify again. Leave the change uncommitted and undeployed.

## Implementation and verification record

- Implemented the labelled-row workaround in Codr. White Noise is unchanged.
- Passed 221 scoped Rust tests, including 11 table regressions, and 174 Python
  tests. Strict Clippy, formatting and whitespace checks passed.
- Both independent read-only reviewers approved the corrected source.
- After separate owner authorisation, deployed an unpublished local candidate
  labelled 0.11.2 on 11 November 2026. The published 0.11.2 release is unchanged.
- Deployment checks preserved the account, 36 chat mappings, receipts, two saved
  drafts and 31 native sessions. Post-deployment native-reader checks passed.
- The owner then confirmed that the phone layout looks good. This confirms the
  observed layout, not every formatting edge case.
- `CHANGELOG.md` records the change under **Unreleased** for the next minor
  release. The source remains uncommitted and unpublished. No version bump.
