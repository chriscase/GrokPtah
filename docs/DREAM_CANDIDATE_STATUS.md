# Dream Candidate Status

This note describes the isolated integration candidate on
`codex/overnight-dream-certification-v1`. It is not a claim that `origin/main` is
complete or that the product has reached 100%.

## Newly certified in this candidate

- Long-horizon durable memory v2 is present with source-workspace addressing,
  project/private-agent/team scopes, host-gated critical writes, revision and
  supersession chains, expiry-aware retrieval, conflict reporting, bounded
  compaction, crash-safe commit cutpoints, replay receipts, and secret-shaped
  evidence redaction.
- The host now exposes replay-safe versioned memory writes to orchestration
  callers, and the model-facing `memory_write` schema accepts explicit
  idempotency, claim, temporal, supersession, and salience fields while keeping
  legacy writes compatible.
- The logical-years certification fixture is typed with deny-unknown fields and
  remains secret-free.
- The certification now covers rollback-safe temporal history and pressure
  compaction without dropping the newly admitted fact.
- Coordinator/worker store certification covers scoped identity, durable
  assignment, manager attribution, message acknowledgement, liveness, and
  restart-safe workload fencing. The workload MCP approval fixture already
  uses a durable Agent identity and rejects omitted or foreign claimants.
- Per-principal worker credential binding is now implemented: a bearer can be
  narrowed to one durable Agent, omitted worker identity resolves to that
  Agent, and cross-agent heartbeat/assignment/message/persistent-agent
  requests fail closed. The published authority capability document carries
  the same Agent scope and a recomputed evidence hash. Production
  issuance/rotation and independent long-running evidence are still open.
- The host now has a least-privilege worker-credential issuance seam: it creates
  an Agent-bound credential with canonical workspace roots and can rotate the
  bearer while preserving the worker identity and scope. The constructor and
  rotation denial/continuity checks pass, and the service can install or
  replace that credential without replacing the primary bearer. Durable
  rotation evidence and the independent worker campaign remain open.
- Computer Use observation now re-checks the durable conflict-domain poison
  fence even for an already-granted Agent, so an uncertain sibling dispatch
  cannot be bypassed by a stale grant.
- A local operator can now reconcile an uncertain physical dispatch only when
  the exact lease, surface, and incarnation match. The durable dispatch stays
  `Uncertain`, the lease is quarantined rather than falsely marked successful,
  the mutation is replay-safe, and the conflict domain is released only after
  that explicit operator confirmation.
- The shared enterprise review admission boundary now validates a disposable,
  secret-free route/model lease, modest-tier gateway attestation, no-fallback
  policy, external egress attestation, read-only permissions, and bounded
  requests/tokens/duration before a live review turn. Route drift, expiry,
  missing attestation, mutation/publication permission, and unknown fields fail
  closed. This is a prerequisite foundation, not a live gateway certification.
- The enterprise review execution contract now freezes seven specialized
  passes, derives per-pass budgets from the admitted route, accepts only safe
  secret-free finding references, deduplicates cross-pass findings, and resumes
  only from a plan-bound checkpoint with full interrupted-attempt history. Its outcome remains explicitly
  `quality_claim_eligible=false` until the live paired campaign is run.
- Each specialist also projects into the provider-neutral durable `WorkTemplate`
  contract with bounded rounds/tokens/time and retry policy; the template carries
  only opaque objective evidence, never a route or raw source prompt. A
  deterministic `EnterpriseReviewWorkPlan` now gives each pass a stable,
  restart-safe idempotency key so an authorized host broker can materialize the
  seven independent workers without duplicate work or provider-specific state.
  The orchestration service now materializes that projection through its
  ordinary scoped durable-work path, replaying plan-bound request IDs on
  partial retries; provider attach and live quality evidence remain separate
  gates.
- The provider-quota receipt contract now requires a named campaign, credential
  and route binding plus distinct provider-side consumption and HTTP-429
  exhaustion observations. Digests, ordering, schema, and secret-free output
  fail closed; this remains a contract for future live evidence, not a live
  quota receipt.
- The operations drill contract now requires all 14 Stage 11 categories,
  combined packaged-desktop/hosted-service evidence, measured RTO/RPO, backup
  confidentiality, and explicit active-target deletion refusal. It remains a
  runbook/report shape, not a dated production-like drill.
- The recurring expert UI/UX review evidence contract now binds an exact
  assembled SHA, packaged-window surfaces/workflows, visual state matrix,
  accessibility checks, and severity disposition. It remains an evidence
  contract; no dated expert review is claimed yet.
- The independent-worker evidence contract now binds exact-candidate
  multi-worker leases, restart/no-duplicate outcomes, per-worker credential
  issuance and rotation, retained audit evidence, and a measured 72-hour soak.
  It remains a contract until a real production-shaped campaign is executed.
- Shared desktop/hosted black-box parity is present as a fail-closed fixture.
  Candidate revisions without an independently captured immutable golden are
  rejected rather than inferred. The local sandbox cannot run its loopback
  gateway, so the candidate parity campaign still needs a host run.

## Verification recorded

### Overnight deterministic rerun — 2026-08-23

The assembled candidate was rerun in an isolated checkout after the UI and
enterprise slices were present:

- Enterprise admission: 4 passed, 0 failed.
- Agent bridge Clippy (`-D warnings`, library target): passed.
- Certification-lab `cargo check --locked`: passed.
- Desktop TypeScript typecheck: passed.
- Desktop Vitest: 46 files, 357 tests passed.
- Desktop production build: passed (352 modules; one existing large-chunk
  advisory remains).
- Certification-lab manifest validation: 33 probes, valid.
- Hermetic provider-behavior replay: 10/10 cases passed with fixture and
  oracle hashes recorded by the lab.

Workspace-wide `cargo fmt --check` still reports only two pre-existing
whitespace-only diffs in `crates/codegen/xai-grok-pager` outside this
candidate's touched surfaces. No formatting change was made to mask that
unrelated signal.

- Coordinator store suite: 12 passed, 0 failed.
- Orchestration library suite: 121 selected tests passed, 0 failed.
- Computer Use library suite: 130 passed, 0 failed, including the uncertain
  dispatch/operator-reconciliation regression; bridge Clippy with `-D
  warnings`: passed after the observation-fence and reconciliation changes.
- Desktop Tauri `cargo check --locked` passed with the cockpit reconciliation
  command/API wired through the host-authorized path.
- The packaged cockpit now renders an accessible exact-surface reconciliation
  panel with bounded operator notes and a fail-closed release action; its
  TypeScript typecheck, full frontend suite (357 tests), and production Vite
  build all pass.
- `memory::tests`: 34 passed, 0 failed.
- Host memory-scope suite: 6 passed, including versioned replay and payload
  conflict rejection.
- Host helper schema test: passed; bridge Clippy with `-D warnings`: passed.
- `memory_long_horizon` integration fixture: 1 passed, 0 failed.
- `grokptah-agent-bridge` Clippy with `-D warnings`: passed.
- Native Coding Readiness/provider-attempt suite: 15 passed, including frozen
  provider routes, quota-backed admission, stale qualification fencing, and
  restart-safe no-duplicate behavior.
- Enterprise review admission unit suite: 5 passed, covering secret-free
  evidence plus expiry, route/model drift, fallback, egress, mutation, bound,
  and schema denials.
- Enterprise review plan suite: 5 passed, covering deterministic seven-pass
  decomposition, checkpoint resume/deduplication, interrupted-pass retry
  history, unsafe-location rejection, plan drift, budget bounds, and
  tampered-admission rejection.
- Provider-quota receipt suite: 4 passed, covering bound-pair readiness,
  route/credential drift, ordering, non-429 exhaustion, tampered digests, and
  unknown fields.
- Operations drill suite: 4 passed, covering combined-environment readiness,
  failed/partial drill denial, duplicate/missing checks, cleanup safety, and
  unknown or malformed evidence.
- Worker lease claimant fencing: bound-credential ownership check passed;
  independent worker leases remained distinct and durable across store reopen.
- Host-issued worker credential lifecycle: focused unit and integration tests
  passed; installation preserves the primary credential and rotation rejects
  the old bearer while retaining the worker id and scope.
- UI review evidence suite: 4 passed, covering complete packaged evidence,
  missing visual states, unresolved P1 findings, malformed digests, unknown
  fields, and missing deferred tracking.
- Independent-worker evidence suite: 4 passed, covering complete multi-worker
  evidence, short-soak and duplicate-execution denial, missing checks,
  credential-rotation denial, and unknown fields.
- Hosted-service security regression: `ServiceConfig` debug output now redacts
  bearer material; the dedicated unit test and service Clippy gate pass.
- Streamable-MCP security transport regression: scoped live-run GET failures
  now use the authority-aware HTTP mapping (403 for forbidden scope, 410 for
  expired cursors, conflict statuses only for actual conflicts/capacity).
  The mapping regression test passes.
- Streamable-MCP now authenticates before parsing request JSON, so malformed or
  oversized unauthenticated input cannot reach the session/protocol parser.
  The unauthenticated-malformed regression is in the host-only loopback suite;
  this sandbox still denies loopback bind, so no live transport result is
  claimed here.
- Long-lived Streamable-MCP event streams now revalidate a secret-free bearer
  fingerprint and authority stamp before emitting frames. Token rotation or
  scope changes terminate the stale stream with a reconnect/recovery signal;
  the worker rotation integration test covers the invalidation path.
- The packaged Computer cockpit approval surface now exposes explicit modal,
  labelled, and described semantics for assistive technology. Its focused
  component suite (15 tests) and desktop TypeScript typecheck pass; live
  packaged visual acceptance is still required.
- Shared black-box fixture: 12 deterministic checks pass; the parity case is
  intentionally blocked here by loopback bind permission and remains open for
  host/CI evidence.
- The review-benchmark contract keeps its fail-closed live behavior; the
  fake-loopback quality path is also host-only in this sandbox because its
  provider must bind `127.0.0.1`.
- The candidate's 10-minute Always-On soak was attempted and stopped before
  execution because the sandbox denied the fake provider's loopback bind
  (`Operation not permitted`); no soak evidence is claimed.

## Still required before a 100% claim

1. Capture the candidate's immutable desktop/hosted parity golden on a host
   where the service gateway can bind, then run the full authenticated fixture.
2. Complete the independent long-running worker outcome: multi-worker crash/
   restart recovery, no duplicate execution, production-shaped issuance and
   rotation of least-privilege credentials, retained evidence, and the
   operational soak.
3. Complete visual acceptance of the packaged reconciliation workflow, then
   execute the new recurring expert review cadence, then complete live Grok
   Build quota/exhaustion evidence, isolated visual Computer Use hardware
   proof, a 72-hour operational soak, and the enterprise-gateway long-running
   review lane.
