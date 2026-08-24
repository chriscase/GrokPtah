# Help Center integration browser check

Date: 2026-08-24

Candidate implementation: `c17e3644129d70d281979f66c75c6618ab22a31a`
(`codex/help-center-integration-v1`), based directly on `origin/main`
`67e29bd34dc64049432c715c93c2cef2185c63ea`.

## Evidence scope

This note records local candidate evidence only. It is not packaged desktop
acceptance, live-provider certification, measured recall/precision, recurring
expert sign-off, or a 100% claim. The independent review did not drive a
browser or re-run the candidate's 46-file / 240-test baseline.

## Candidate checks recorded

- The real sidebar Help action opens a labelled Help Center dialog.
- The dialog presents twelve source-backed article IDs, including the Grok
  Build/Grok Bot boundary article, topic filtering, offline lexical retrieval,
  source cards, and the optional assistant boundary notice.
- Source/tests cover initial search focus, Tab/Shift+Tab wrapping inside the
  dialog, Escape closing or cancelling a nested confirmation, and focus return
  to the opener.
- At 720px × 800px, the dialog and source card remain usable without
  horizontal overflow.
- Meaning search is disabled until a query exists and names the configured
  provider before sending only article metadata.
- Source locators are checked against headings present at this candidate HEAD;
  no absent handoff path is presented as a source-backed citation.
- `npm audit --audit-level=high` reports zero vulnerabilities; no runtime
  dependency was added.

## Not yet evidenced

- Packaged desktop keyboard/forced-colors acceptance after the final correction.
- A live semantic/assistant provider campaign with redacted receipts.
- Recall/precision measurements on a larger corpus.
- The recurring accessibility and product-copy expert review cadence.
