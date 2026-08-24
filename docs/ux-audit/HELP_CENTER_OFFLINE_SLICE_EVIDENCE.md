# Help Center offline slice — evidence note

Status: isolated UX candidate; not integrated into `origin/main`, not packaged
acceptance, and not a 100% claim.

## Candidate identity

- Candidate worktree: `/private/tmp/grokptah-ui-contrast-fix`
- Candidate revision: the exact commit containing this evidence note; record
  `git rev-parse HEAD` when assembling the packaged review packet.
- Working tree: clean after the candidate commit
- Date: 2026-08-24

## Surface delivered

- Visible Help entry in the sidebar and `/help` command routing.
- Full-screen accessible dialog with labelled search, topic filter, Escape
  close, focus-visible styling, responsive narrow layout, and reduced-motion
  handling.
- Eleven stable offline article IDs covering sessions, search, provider
  routes, restricted-company reviews, live gateway evidence, Computer Use
  boundaries, isolated guests, multi-agent coordination, general evidence,
  the always-on soak, and the grounded Help assistant.
- Deterministic field-weighted retrieval with explicit aliases for power-user
  paraphrases, transparent plural/diacritic normalization, and exact terms
  outranking prose matches.
- Versioned retrieval fixtures cover exact identifiers, paraphrases, topic
  filters, unsupported questions, and natural-language stop-word handling.
- Every article carries one or more explicit source IDs, repository paths, and
  headings rendered in the source card.
- Retrieval is built from a stable `HELP_INDEX` contract, and the candidate
  now includes an optional provider-semantic ranking path that preserves the
  same article IDs and validates every returned score against that corpus.
- Offline results carry `offline-lexical`; provider-ranked results carry
  `provider-semantic`. The UI never silently represents one mode as the other.
- The candidate now exposes a source-only grounded-assistant request contract
  with corpus version, retrieval mode, explicit confirmation, and citation
  validation, plus a confirmation-gated provider callback through a fresh chat
  session. Search results also expose an explainable heuristic confidence
  signal; it is explicitly not a model-confidence or certification claim.
  Live provider routing and qualification remain unverified.
- Honest no-result state and source/evidence note on every displayed article.

## Verification

From `/private/tmp/grokptah-ui-contrast-fix/desktop`:

- `npm run typecheck` — pass.
- `npm test -- --reporter=dot` — 48 files, 376 tests passed.
- `npm run build` — pass; Vite production bundle emitted.
- `git diff --check` — pass.

## Browser visual pass

- Local Vite preview checked at the default viewport and explicit 720×800
  narrow viewport.
- The natural-language query `why is the company gateway model weak?` ranks
  the provider-route article first after stop-word filtering; no horizontal
  overflow was observed at 720px (`scrollWidth == innerWidth`).
- The meaning-search control is disabled until a query exists, then exposes a
  clear confirmation dialog naming the configured provider and stating that
  only static article metadata will leave the app. No provider request was
  sent during this browser pass.
- The final expanded-corpus pass exposed eleven article buttons, kept the
  restricted-company review article first for the natural-language gateway
  query, and preserved the same confirmation/no-overflow behavior at 720px.

## Remaining gates

This slice still needs live provider qualification of the semantic route,
packaged desktop acceptance, and the recurring expert review cadence before it
can be promoted.
