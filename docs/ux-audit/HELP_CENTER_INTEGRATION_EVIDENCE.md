# Help Center integration evidence

Status: integrated candidate evidence; packaged acceptance, expert sign-off,
live provider qualification, and 100% release qualification remain open.

## Candidate

- Code head: `da10ba77` (Help Center integration, public consumer package,
  broker-boundary follow-ups, and streamed-event redaction)
- Public contract follow-up: `c7800a46` (`feat(public): expose source-cited Help Center contract`)
- Styling correction: `f9eee072` (`fix(ui): apply Help Center surface styles`)
- Consent layering correction: `1235406d` (`fix(ui): keep consent above Help Center`)
- Base: current GrokPtah `main` at integration time
- Scope: Help Center UI, deterministic source-cited corpus, provider-semantic
  retrieval contract, bounded assistant contract, validation, and cleanup

## Delivered behavior

- Eighteen stable article IDs cover sessions, search, providers, restricted
  gateways, Grok Build boundaries, Computer Use, coordination, durable
  recovery, prompt queues, MCP, broker embedding, review receipts, Always-On
  operation, and help-assistant limits.
- Offline lexical search is deterministic and labelled as offline; aliases and
  weighted fields support power-user paraphrases.
- Meaning search and assistant requests require explicit confirmation, send
  only bounded metadata/cited context, preserve corpus IDs, validate strict
  JSON answers, cap provider answer/ranking sizes and scores, and delete their
  helper chat session after completion.
- The full-screen dialog traps focus, restores focus on close, handles Escape,
  supports narrow layouts, reduced motion, forced colors, and visible focus.

## Verification run

- `npm test`: 47 files / 276 tests passed.
- `npm run typecheck`: passed.
- `npm run build`: passed; Vite production bundle emitted.
- `npm run verify:public`: passed; browser-safe public bundle, authority checks,
  an installed `npm pack` archive, and a disposable external-consumer import
  through normal `node_modules` resolution remained green, including `searchHelpArticles`, the exported
  `HELP_ARTICLES` corpus, and the separate `@grokptah/client/ui-core` subpath.
- `git diff --check`: passed.

## Remaining gates

1. Expert accessibility/product review against the packaged desktop app.
2. Larger-corpus recall/precision and live provider receipts with no silent
   fallback.
3. ContextDesk desktop and War Room disposable end-to-end integration.
