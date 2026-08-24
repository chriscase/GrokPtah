# Help Center integration browser check

Date: 2026-08-24

Candidate: `f2644f23` (`codex/help-center-integration-v1`), based directly on
`origin/main` `67e29bd34dc64049432c715c93c2cef2185c63ea`.

## Checks

- Started the desktop preview from the integration checkout.
- Opened the real sidebar Help action and verified the accessible `Help Center`
  dialog.
- Verified the dialog presents eleven source-backed articles, topic filtering,
  the offline lexical retrieval label, source cards, and the optional assistant
  boundary notice.
- At a 720px × 800px viewport, expanded the Lanes rail, reopened Help, and
  verified `document.documentElement.scrollWidth == window.innerWidth` (720px),
  with the dialog and source card still present.
- Reset the temporary viewport override and closed the preview tab/server.

This is local browser evidence for the isolated integration candidate. It is
not packaged desktop acceptance, live-provider certification, or expert UI
sign-off.
