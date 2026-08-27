# Help Center integration evidence

Status: integrated candidate evidence; packaged acceptance, expert sign-off,
live provider qualification, and 100% release qualification remain open.

## Candidate

- Code head: `cc151fcc` (Help Center integration, public consumer package,
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

## Authority-backed consumer (follow-up)

The Help Center now consumes the canonical Help authority
(`grokptah.help-authority.v1`) through a separate presentation contract,
`grokptah.help-center-view.v1`. See
[`HELP_CENTER_CONSUMER_CONTRACT.md`](../HELP_CENTER_CONSUMER_CONTRACT.md).

- Retrieval is the authority's offline hybrid search over one canonical
  23-article corpus; the previous lexical scorer is no longer the UI's source
  of truth.
- Answer, ambiguous, low-confidence, no-match, rejected, and browse are
  distinct rendered states. `answer` is the only state carrying an answer, and
  a rejected query is worded as a rejection rather than an abstention.
- Citation spans are re-resolved against the corpus before rendering; a span
  the corpus does not reproduce is dropped and the drop is disclosed.
- Access, audience, and documented capabilities are labelled, each capability
  carrying `live: unknown`, so a documented capability cannot read as an
  available one.
- Search is a combobox/listbox with `aria-activedescendant`, arrow/Home/End/
  Enter handling, and no focus movement out of the search field. Escape still
  belongs to the dialog. Focus trapping, opener restoration, background inert,
  and the consent-layer exception are unchanged.
- Contrast: theme tokens checked at AA against the panel background, state
  never carried by colour alone, plus `prefers-contrast: more` and extended
  `forced-colors` handling.
- Retrieval shows no loading state, because it performs no I/O. The optional
  model seam enforces its declared `timeoutMs`, aborts the adapter, reports the
  timeout against the declared budget without asserting an elapsed time, and
  keeps provider, model, cost, and latency `unknown`.
- Legacy compatibility: `helpCenter.ts` is unedited, and the existing
  `onAskAssistant` / `onSearchSemantic` / `assistantProviderLabel` props still
  work. Provider ranking may now only reorder candidates, never replace the
  result set.
- Fixtures are a synthetic five-article corpus, so UI expectations do not drift
  when shipped documentation is edited.

### Verification run (follow-up)

- `npx vitest run src/lib/helpAuthority.test.ts src/lib/helpAnswer.test.ts
  src/lib/helpCenterView.test.ts src/lib/helpCenter.test.ts src/lib/help.test.ts
  src/components/HelpCenter.test.tsx src/components/HelpPanel.test.tsx`: passed.
- `npm test`, `npm run typecheck`, `npm run build`, `npm run verify:public`:
  see the branch report for exact counts at the recorded head.

## Remaining gates

1. Expert accessibility/product review against the packaged desktop app.
2. Larger-corpus recall/precision and live provider receipts with no silent
   fallback.
3. ContextDesk desktop and War Room disposable end-to-end integration.
4. Screen-reader verification of the combobox/listbox announcement path on each
   supported platform; the automated tests assert the ARIA wiring, not what a
   given screen reader speaks.
