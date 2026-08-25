# Help Center integration evidence

Status: integrated candidate evidence; packaged acceptance, expert sign-off,
live provider qualification, and 100% release qualification remain open.

## Candidate

- Code head: `45441d1f` (`feat(desktop): integrate evidence-backed Help Center`)
- Public contract follow-up: `c7800a46` (`feat(public): expose source-cited Help Center contract`)
- Styling correction: `f9eee072` (`fix(ui): apply Help Center surface styles`)
- Consent layering correction: `1235406d` (`fix(ui): keep consent above Help Center`)
- Base: current GrokPtah `main` at integration time
- Scope: Help Center UI, deterministic source-cited corpus, provider-semantic
  retrieval contract, bounded assistant contract, validation, and cleanup

## Delivered behavior

- Twelve stable article IDs cover sessions, search, providers, restricted
  gateways, Grok Build boundaries, Computer Use, coordination, evidence,
  Always-On operation, and help-assistant limits.
- Offline lexical search is deterministic and labelled as offline; aliases and
  weighted fields support power-user paraphrases.
- Meaning search and assistant requests require explicit confirmation, send
  only bounded metadata/cited context, preserve corpus IDs, validate strict
  JSON answers, and delete their helper chat session after completion.
- The full-screen dialog traps focus, restores focus on close, handles Escape,
  supports narrow layouts, reduced motion, forced colors, and visible focus.

## Verification run

- `npm test`: 47 files / 261 tests passed.
- `npm run typecheck`: passed.
- `npm run build`: passed; Vite production bundle emitted.
- `npm run verify:public`: passed; browser-safe public bundle and consumer
  authority checks remained green, including `searchHelpArticles` and the
  exported `HELP_ARTICLES` corpus.
- `git diff --check`: passed.

## Remaining gates

1. Expert accessibility/product review against the packaged desktop app.
2. Larger-corpus recall/precision and live provider receipts with no silent
   fallback.
3. ContextDesk desktop and War Room disposable end-to-end integration.
