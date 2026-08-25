# Roadmap to 100%

**Current claim:** GrokPtah is not yet 100%. The project has a substantial
coding-agent/control-plane foundation, but several qualification and
cross-product gates remain open.

“100%” means every stage below has a dated, reproducible exit artifact. A
feature being present in source, or a draft PR being green on one job, is not
enough to close a stage.

## Ordered stages

| Stage | Outcome | Current evidence | Status |
| --- | --- | --- | --- |
| 1. Agent foundation | Desktop bridge, sessions, permissions, streaming, tools, MCP | [Architecture](./ARCHITECTURE.md), [MCP coordinator](./MCP_CONTROL_COORDINATOR.md) | Substantially shipped |
| 2. Durable operation | Durable runs, queues, checkpoints, explicit continuation, isolated review | [Durable runs](./DURABLE_RUNS.md), [persistent protocol](./PERSISTENT_AGENT_PROTOCOL.md) | Shipped with release gates |
| 3. Always-On continuity | Restart-safe, duplicate-safe, multi-worker long-running operation | Protected Stage 6 soak is running; final artifact and post-soak campaigns are still absent | In progress |
| 4. Gateway and enterprise | Least-privilege gateway profiles, quota/route truth, review through restricted company gateways | [Provider profiles](./PROVIDER_PROFILES.md); live enterprise certification remains unverified | In progress |
| 5. Safe Computer Use | Redacted observation, semantic action, isolation, leases, stale-frame rejection, coordination | [Computer Use design](./COMPUTER_USE.md); source proof exists, packaged VM/hardware proof remains | In progress |
| 6. Operator UX | Fast power-user workflows, keyboard/screen-reader correctness, understandable approvals and recovery | The integrated Help Center at `45441d1f` adds focus trapping, Escape/focus restoration, responsive layout, reduced-motion and forced-colors handling; full desktop suite is 47 files / 262 tests; independent expert acceptance remains | In progress |
| 7. Semantic help | Searchable in-app help with contextual guidance and an optional assistant boundary | [`HelpCenter`](../desktop/src/components/HelpCenter.tsx) and [`helpCenter.ts`](../desktop/src/lib/helpCenter.ts) provide 18 stable source-cited articles spanning core operation, recovery, queue steering, MCP, broker embedding, Computer Use, and providers, with deterministic paraphrase search, explicit offline/provider retrieval labels, confirmation-gated semantic ranking, bounded assistant context and provider-response ceilings, strict answer validation, and helper-session cleanup; live provider/corpus qualification remains | In progress |
| 8. Embeddable platform | Stable Rust DTOs, desktop adapter, browser-safe broker, reusable UI primitives | [Cross-product ADR](./ADR-003-cross-product-capability-surface.md), [embedding guide](./EMBEDDING.md), [broker protocol](./WEB_BROKER_PROTOCOL.md), [SDK](../crates/common/grokptah-agent-sdk/README.md) | Contract, response-validation, headless UI, source-cited Help Center exports (`c7800a46`), hardened browser authority checks (`b17e901f`), installable Tauri-free package staging, and normal `node_modules` consumer smoke checks are verified; publication and cross-repository integration remain |
| 9. Independent qualification | Strongest-model code/security/UI review, cross-language conformance, soak, gateway, packaged-CU, and recovery evidence | [Independent review protocol](./INDEPENDENT_REVIEW_PROTOCOL.md) | Pending exact candidate head |
| 10. Release and adoption | Versioned packages, examples, migration docs, reproducible builds, signed artifacts, release runbook | Public TypeScript package staging and packaging instructions exist; signed artifacts, hosted-service release, migration/changelog, and final runbook remain | In progress |

## The 100% exit gate

The project can claim 100% only when all of these artifacts exist and point to
the same candidate release:

1. Always-On soak completes at the configured duration with no duplicate sends,
   uncertain resumes, leaked workers, or missing terminal evidence.
2. Gateway/enterprise campaign proves restricted-company-gateway review,
   provider identity, quota truth, retries, and auditability.
3. Computer Use proves packaged helper/image launch, real guest boot, rendered
   frames, host input, cleanup, lease expiry, stale revision denial, and soak
   on the supported hardware matrix.
4. UI review passes keyboard, screen reader, focus, contrast, reduced motion,
   large text, and power-user throughput checks in a real packaged app.
5. Help search returns useful, permission-safe results across the full shipped
   capability set; any assistant is bounded by the same authority contract.
6. ContextDesk desktop and War Room integrations pass disposable end-to-end
   tests without giving the browser desktop authority or raw credentials.
7. Independent Cursor/Claude reviews pass against the exact candidate/base
   SHAs, with all findings resolved or explicitly accepted.
8. Published Rust/TypeScript package artifacts, schemas, examples, changelog,
   migration notes, and signed/reproducible release outputs are available.

## Current blockers and next actions

- Preserve the protected Stage 6 soak until it emits its final artifact; do not
  start another Cargo build wave on its shared target.
- Keep the current TypeScript/desktop verification attached to the same reviewable
  candidate: `45441d1f` passes 47 test files / 262 tests, typecheck, and the
  production Vite build; `b8338cc9` plus the external-consumer fixture additionally
  pass `npm run verify:public` and `npm pack --dry-run` with generated
  export/authority-boundary and package-resolution smoke checks. The package
  remains a release candidate until publication and a real cross-repository
  consumer integration are green.
- Put the current integration changes on an exact reviewable candidate, then
  run the independent strongest-model review protocol with Fast off.
- Integrate ContextDesk's server broker and desktop adapter against the SDK and
  add cross-repository conformance fixtures.
- Resolve the open UI/help, orchestration-race, packaged Computer Use, and
  always-on certification PRs only after their evidence gates pass.
- Run the full release gate against one candidate; do not infer 100% from the
  number of merged PRs or the presence of source-level types.
