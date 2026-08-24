# Dream Candidate Status

This note describes the isolated integration candidate on
`codex/cu-isolated-guest-bootstrap-v1`. It is not a claim that `origin/main` is
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
- The candidate enforces a role-scoped authority ceiling for hosted bearers:
  `RemoteCoordinator`, explicit `RemoteOperator`, and `Observer` credentials
  expose different operation sets, while only the trusted local adapter gets
  `LocalOperator` authority. Computer-read capability is an immutable,
  session/workspace-bound grant; no bearer can widen it through MCP arguments.
  Production-shaped credential issuance and the retained worker campaign are
  still open.
- The current dream candidate adds the exact-head Stage 3 `authority`
  campaign. Its deny-unknown report binds the four-role
  contract, explicit RemoteCoordinator/Observer denials, authority-bound
  idempotency, Agent-bound worker identity, immutable scoped Computer reads,
  the public capability document, and hash-bound host profiles to seven ordered
  gate families / 22 tests
  on one clean SHA. The added behavioral MCP gate proves authorized reads and
  indistinguishable cross-session/cross-workspace/unknown-run denial. Failed
  gates retain only a bounded digest checkpoint and cannot seal. The product
  tests now directly prove that bearer credentials cannot mint
  `LocalOperator` and that Observer lacks the named mutation set.
  This slice is formatted and statically reviewed but deliberately **not
  claimed tested or certified** until the mandatory external sccache/target
  runner executes it, including its loopback gates.
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
- The loopback MCP control plane now exposes `ptah_create_enterprise_review`.
  It accepts only the signed lease, separate operator public trust, and
  secret-free repository/scope fingerprints; it verifies gateway trust before
  materializing the seven passes and is guarded by the existing `WorkCreate`
  authority. Unsigned or caller-prebuilt plans cannot enter through this
  surface.
- Signed enterprise review identity now survives all the way to native Run
  admission. Every projected WorkItem freezes provider, model, endpoint,
  canonical credential-principal, and route-binding fingerprints; managed
  selection rejects provider/model drift, and the exact pre-Run provider
  snapshot rejects endpoint/credential drift, offline execution, and fallback.
  The constraint also participates in the idempotency payload. This closes the
  prior gap where signed admission could materialize durable work whose Agent
  selection later resolved through mutable provider state.
- The provider-quota receipt contract now requires a named campaign, credential
  and route binding plus distinct provider-side consumption and HTTP-429
  exhaustion observations. Digests, ordering, schema, and secret-free output
  fail closed; this remains a contract for future live evidence, not a live
  quota receipt.
- `LiveProviderCampaignEvidence` now binds a ready Grok Build attestation to
  the complete quota receipt pair with canonical credential/route digests and
  its own transport-tamper digest. It is an evidence artifact shape, not a
  claim that a live campaign has run.
- Enterprise durable work-plan deserialization now denies unknown nested
  `WorkTemplate` fields, with a regression covering a tampered policy member;
  the broker cannot silently discard an unrecognized field before validation.
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
- Desktop and service control-plane entry paths now bind distinct
  `desktop_local` / `standalone_service` host assertions, opaque stable host
  instance identity, bridge version, and exact capability sets into the
  initialize document without expanding the bearer role. The fixture checks
  initial and post-restart stability and typed denial for an undeclared
  Computer mutation. No current-head immutable golden or Stage 4 certificate
  is claimed yet.
- Certification-lab shutdown now awaits each Tokio worker exactly once; the
  prior timeout-then-second-await path could panic with `JoinHandle polled
  after completion`. The offline lab suite is now a clean 92/92, but this is
  harness/restart reliability evidence, not a live or 72-hour soak claim.

### Latest desktop safety continuation — 2026-08-24

Candidate `af609a4278f71998a58e9f352fdc3b2795281d94` extends the shared,
bounded backend-error display boundary across the desktop surfaces. Credential-
shaped values, local paths, and UI-only secret placeholders are redacted before
errors reach search, session, run, routine, worker, settings, terminal, remote
agent, provider-readiness, or Computer Run UI. Computer Run storage contention
also has a clear retry path. The focused redaction tests and full desktop suite
(52 files, 389 tests) pass; this is source/UI safety evidence only and is not a
packaged-desktop acceptance or expert UX cadence record. Parent-provided load
diagnostics now pass through the same boundary before technical details render,
closing the path visible in the earlier persistent-agent capture. The visual
review also found unlabeled glyph-only operator controls; Work refresh, new-tab,
shell-dismiss, and Live-rail-hide controls now expose explicit accessible names
with regression coverage. Search and modal Settings now keep Tab traversal
inside the true modal surface, with a shared focus-trap helper and component
regression coverage, and closing either surface returns focus to its opener.
The full desktop suite now passes 52 files / 389 tests. The App-owned activity,
transcript, durable-work, lane-scope, remote-connection, and rate-limit error
paths now use the same bounded sanitizer, and the selected-lane blocked alert
has a regression for path/credential leakage. The global React error boundary
now uses that same sanitizer before showing render diagnostics, with a regression
for path/credential leakage there as well. Durable Run timeline error and rate-
limit events now use the same bounded display path, with a timeline regression.
The shared sanitizer also covers ordinary `/tmp`, `/private/var`, and mounted
volume paths that can appear in native/runtime failures.
This remains source/UI evidence, not packaged review.

## Verification recorded

### Overnight deterministic rerun — 2026-08-23

The assembled candidate was rerun in an isolated checkout after the UI and
enterprise slices were present:

- Enterprise admission: 4 passed, 0 failed.
- Full `grokptah-agent-bridge` library suite: 678 passed, 0 failed;
  signed-attestation verification and all existing Computer Use, memory, and
  orchestration regressions are included.
- Agent bridge Clippy (`-D warnings`, library target): passed.
- Native executor MCP integration suite: 19 passed, 0 failed; the signed-route
  drift regression records zero provider requests, zero Runs, and no live
  intent when endpoint or credential identity differs at admission.
- Agent bridge Clippy (`-D warnings`, all targets): passed.
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
- `memory_long_horizon` integration fixture: 2 passed, 0 failed.
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
- Authority role and Computer-read fencing: 16 filtered authority/host tests
  passed, including observer read-only ceilings, remote-operator denial of
  Computer Use, scoped Computer-read grants, stale/revoked authority, and
  frozen AgentSpec authority.
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
- The certification lab now accepts a bounded
  `GROKPTAH_ENTERPRISE_REVIEW_LEASE` file and re-admits the existing
  secret-free enterprise review contract. It now also requires a separate
  operator-selected `GROKPTAH_ENTERPRISE_REVIEW_TRUST` file and verifies the
  lease's detached Ed25519 gateway signature. Missing, stale, malformed,
  symlinked, oversized, unsigned, incorrectly signed, or broadened material
  fails closed; a valid lease still produces only an indeterminate live report
  until real provider usage, restart, and paired-quality evidence are captured.
- A typed, digest-bound `MemoryLongHorizonEvidence` contract now exists for
  the deterministic logical-years campaign. It requires the three memory
  scopes, ten logical years, exact quality oracles, storage/reopen bounds, and
  a secret-free evidence digest. The retained deterministic artifact is
  [`docs/evidence/memory-long-horizon-campaign-v1.json`](evidence/memory-long-horizon-campaign-v1.json)
  at candidate `47d7f71`; it deliberately has `claim_eligible: false`.
- Code slice `96c28cec36002785a8a03ca5d5d3dca1dbfa78f0` closes the Manager
  frozen-memory implementation gap. Each decision occurrence now owns a
  deny-unknown attribution over the exact AgentSpec revision, canonical memory
  policy, source workspace, bounded quoted project context, and decision Work
  objective. The directive must echo that attribution digest; later objective,
  policy, spec, or context drift fails closed. Proposal-only Runs suppress
  ambient memory injection, so later project facts cannot silently change the
  reasoning input, and objective drift is rejected before provider admission.
- Exact-slice qualification: bridge library `680 passed`; manager-store restart
  suite `5 passed`; focused Manager attribution/admission tests passed; bridge
  all-target Clippy passed with warnings denied. The socket-backed supervisor
  and native-provider suites compile, but their current execution is not
  claimed because this sandbox refused `127.0.0.1:0` listener creation before
  the test body. The retained logical-years artifact must be recaptured by an
  integrated host/CI campaign before Stage 5 is called certified.
- Candidate `a530f20d59d64b1d9825690c45c553a1c4191852` adds the exact-head
  Stage 5 `memory` runner. Its ten-gate manifest binds the fresh logical-years
  payload, scope/crash/restart checks, frozen Manager attribution/objective,
  durable Manager-store recovery, and the host-only supervisor/native proofs
  into one deny-unknown sealed report. Certification-lab qualification is
  `97 passed`; strict all-target Clippy passes for both lab and bridge; the
  Manager store is `5 passed`. The full bridge run produced `643 passed` plus
  36 loopback/socket-denied failures in this sandbox; one parallel freshness
  timing case passed when rerun alone. A local runner exercise failed closed
  with no report or completion seal, so no Stage 5 certification is claimed.
- Rust campaign builds now require explicit
  `RUSTC_WRAPPER=/opt/homebrew/bin/sccache`, the namespaced GrokPtah sccache
  directory, and a Rust/toolchain/feature-compatible external
  `CARGO_TARGET_DIR`. The prior in-checkout bridge and lab targets were inactive
  and removed after open-handle checks; future compatible runs reuse the
  external target serially.
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
