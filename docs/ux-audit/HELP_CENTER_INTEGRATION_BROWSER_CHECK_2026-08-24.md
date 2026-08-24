# Help Center integration browser check

Date: 2026-08-24

Candidate implementation: `c17e3644129d70d281979f66c75c6618ab22a31a`
(`codex/help-center-integration-v1`), based directly on `origin/main`
`67e29bd34dc64049432c715c93c2cef2185c63ea`.

## Evidence scope

This note records local candidate evidence only. It is not packaged desktop
acceptance, live-provider certification, measured recall/precision, recurring
expert sign-off, or a 100% claim. The independent review did not drive a
browser or re-run the candidate's 46-file / 244-test baseline.

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

## Current-head browser recheck

The corrected implementation head `f35813ef` was opened in the isolated local
preview with the in-app browser. At the default dark viewport and 720×800
narrow viewport:

- the Help dialog opened from the real sidebar entry and placed focus in
  Search help;
- 12 article options rendered, with the restricted-company review ranked first
  for `why is the company gateway model weak?`;
- the source card showed `docs/PROVIDER_PROFILES.md · Provider profiles`;
- the offline lexical label and non-certification confidence note were visible;
- `document.documentElement.scrollWidth == window.innerWidth` at 720px; and
- Escape closed the dialog without leaving it open.

This remains local browser evidence; packaged desktop, forced-colors, and
independent expert acceptance are still open.

## Not yet evidenced

- Packaged desktop keyboard/forced-colors acceptance after the final correction.
- A live semantic/assistant provider campaign with redacted receipts.
- Recall/precision measurements on a larger corpus.
- The recurring accessibility and product-copy expert review cadence.
