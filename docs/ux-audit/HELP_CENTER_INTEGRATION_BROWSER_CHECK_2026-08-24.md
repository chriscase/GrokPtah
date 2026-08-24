# Help Center integration browser check

Date: 2026-08-24

Candidate: `f2644f23` (`codex/help-center-integration-v1`), based directly on
`origin/main` `67e29bd34dc64049432c715c93c2cef2185c63ea`.

## Checks

- Started the desktop preview from the integration checkout.
- Opened the real sidebar Help action and verified the accessible `Help Center`
  dialog.
- Verified the dialog presents twelve source-backed articles, including the
  Grok Build/Grok Bot boundary article, topic filtering,
  the offline lexical retrieval label, source cards, and the optional assistant
  boundary notice.
- Verified the modal starts focus in Search help, wraps Tab/Shift+Tab within
  the dialog, closes on Escape, and restores focus to the opener on unmount.
- At a 720px × 800px viewport, expanded the Lanes rail, reopened Help, and
  verified `document.documentElement.scrollWidth == window.innerWidth` (720px),
  with the dialog and source card still present.
- Reset the temporary viewport override and closed the preview tab/server.
- Refreshed the lockfile so the Vite/PostCSS development toolchain resolves
  `nanoid@3.3.18` and reran the locked dependency audit. Production and full
  audits now report zero vulnerabilities; no runtime dependency was added.

This is local browser evidence for the isolated integration candidate. It is
not packaged desktop acceptance, live-provider certification, or expert UI
sign-off.
