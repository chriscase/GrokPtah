# Help Center offline slice — evidence note

Status: isolated UX candidate; not integrated into `origin/main`, not packaged
acceptance, and not a 100% claim.

## Candidate identity

- Candidate worktree: `/private/tmp/grokptah-help-integration-v1`
- Candidate branch: `codex/help-center-integration-v1`
- Candidate implementation revision: `c17e3644129d70d281979f66c75c6618ab22a31a`
- Base: `origin/main` `67e29bd34dc64049432c715c93c2cef2185c63ea`
- Working tree: clean after the candidate commit
- Date: 2026-08-24

The evidence-note commit is part of this same clean candidate branch; record
`git rev-parse HEAD` when assembling the promotion packet.

## Surface delivered

- Visible Help entry in the sidebar and `/help` command routing.
- Full-screen dialog with labelled search, topic filter, Escape close, focus
  return, responsive narrow layout, forced-colors styling, and reduced-motion
  handling.
- Twelve stable offline article IDs:
  `getting-started.sessions`, `getting-started.search`, `providers.gateway`,
  `providers.live-gateway-evidence`, `providers.grok-build-boundary`,
  `providers.restricted-gateway-review`, `computer-use.boundaries`,
  `computer-use.isolated-guest`, `computer-use.multi-agent-coordination`,
  `operations.evidence`, `operations.always-on-soak`, and
  `operations.help-assistant`.
- Deterministic field-weighted retrieval with explicit aliases for power-user
  paraphrases, transparent plural/diacritic normalization, and exact terms
  outranking prose matches.
- Versioned retrieval fixtures cover exact identifiers, paraphrases, topic
  filters, unsupported questions, and natural-language stop-word handling.
- Every article carries source IDs, repository paths, and headings that exist
  at this candidate revision. The isolated-guest article explicitly says that
  source-level proof does not qualify a packaged VM or usable guest surface.
- Optional provider-semantic ranking and grounded-assistant requests are
  confirmation-gated, metadata/context bounded, corpus-versioned, and
  validated. Helper chat sessions are deleted after each completed request.

## Verification

Candidate evidence reports the following from the exact worktree:

- `npm run typecheck` — pass.
- `npm test -- --reporter=dot` — 46 files, 240 tests passed.
- `npm run build` — pass; Vite production bundle emitted.
- `npm audit --audit-level=high` — zero vulnerabilities.
- `git diff --check` — pass.

The independent review did not re-run this full baseline; it remains evidence
to repeat at promotion time.

## Browser visual pass

Prior local browser evidence covered the real Help dialog, twelve article
buttons, topic/search behavior, source cards, and a 720px narrow viewport. That
evidence predates the final boundary/accessibility correction and must be
re-run for packaged promotion. It is not packaged desktop acceptance.

## Remaining gates

1. Live provider qualification of the confirmation-gated semantic route and
   grounded assistant, with receipts and no silent fallback.
2. Measured recall/precision against a larger corpus than the deterministic
   fixtures.
3. Packaged desktop acceptance, including keyboard and forced-colors checks.
4. Recurring expert review cadence for accessibility and product copy.
