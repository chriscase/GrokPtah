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
- Verified the modal starts focus in Search help, wraps Tab/Shift+Tab within
  the dialog, closes on Escape, and restores focus to the opener on unmount.
- At a 720px × 800px viewport, expanded the Lanes rail, reopened Help, and
  verified `document.documentElement.scrollWidth == window.innerWidth` (720px),
  with the dialog and source card still present.
- Reset the temporary viewport override and closed the preview tab/server.
- Ran the locked dependency audit. The only advisory is high-severity
  `nanoid <3.3.18` through the Vite/PostCSS development toolchain; the
  production-only audit reports zero vulnerabilities. This remains a
  development-toolchain maintenance item, not a runtime Help Center
  dependency.

This is local browser evidence for the isolated integration candidate. It is
not packaged desktop acceptance, live-provider certification, or expert UI
sign-off.
