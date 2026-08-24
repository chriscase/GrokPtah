# Road to 100%

This is the dependency-ordered path from `origin/main` to a **trustworthy**
100% claim. It does not describe 100% as already met.

Status of what exists today is
[`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md). Do not treat draft
[PR #352](https://github.com/chriscase/GrokPtah/pull/352) or
[#343](https://github.com/chriscase/GrokPtah/pull/343)–[#351](https://github.com/chriscase/GrokPtah/pull/351)
as shipped. **Stage 1 cannot pass while PR #352 remains draft.**

## Evidence baseline (pinned)

Issue-state and draft-PR evidence below is dated **2026-08-22 (UTC)**. Re-audit
before treating this document as current.

| Pin | SHA | Role |
| --- | --- | --- |
| `origin/main` | `67e29bd34dc64049432c715c93c2cef2185c63ea` | Shipped-on-main baseline. Objects not on this SHA are not Supported. |
| Inspected [PR #352](https://github.com/chriscase/GrokPtah/pull/352) head | `4bd2081b2945e8ce881895f976bb7c8d88b929f2` | Native Coding RC. **Pending — not shipped.** Independently confirmed P1s on this head block merge (stage 1). |
| This matrix/roadmap parent ([PR #353](https://github.com/chriscase/GrokPtah/pull/353) head) | `e5828740e1bb6d36953ac8a44ef48a08eafc03e6` | Documentation-only capability matrix. Not a 100% claim. |

**Evidence kinds — do not collapse:**

| Kind | What it can prove | What it cannot prove |
| --- | --- | --- |
| **Deterministic** | In-tree unit, integration, and protocol tests on a named SHA | Live provider behavior, packaged TCC identity, years-long retention, 72-hour hosted operations |
| **Hardware** | Packaged-identity Computer Use, Screen Recording / Accessibility, display/focus matrices ([#274](https://github.com/chriscase/GrokPtah/issues/274)) | Logical-years memory, Grok Build account balance, least-privilege production tokens |
| **Live-provider** | Named Grok Build campaign with `certification_ready == true`, required catalog IDs, **and** a named secret-free provider-quota receipt (campaign/credential/route-bound consumption **and** exhaustion/429) | GrokPtah local host quota ledger; Grok Build account-balance sync (not implemented, **not** a 100% requirement); isolated visual Computer Use |
| **Soak** | Dated 72-hour sole-writer hosted operations (restarts, resource bounds, no implicit resume) | Accelerated logical-years memory; elapsed wall-clock soak is not a memory proof |

Named deterministic artifacts on `origin/main` `67e29bd34dc64049432c715c93c2cef2185c63ea`
include: `crates/codegen/grokptah-agent-bridge/tests/computer_use_release_gate.rs`
(`unsupported_pointer_fallback_never_reaches_backend`, MCP read-only Computer
surface); `crates/codegen/grokptah-agent-bridge/src/orchestration/store.rs`
(`restart_marks_running_interrupted`); `tests/manager_supervisor.rs`;
`tests/manager_mcp.rs`; `tests/memory_scopes.rs`;
`tests/native_executor_store.rs`; `tests/native_executor_mcp.rs`;
`tests/orchestration_control.rs`; `crates/codegen/grokptah-service/tests/service_conformance.rs`;
`src/auth_store.rs` `resolve_xai_credentials`; certification-lab hermetic
`evals/certification-lab/replay-fixtures/provider-behaviors.v1.json`.
Hardware, live-provider, and 72-hour soak reports are **absent**. PR #352 tests
(`native_coding_readiness.rs`, `ProviderReadinessCenter.test.tsx`,
`soak_restart_recovery_matrix`, `.github/workflows/hosted-service.yml`) remain
**Pending — not shipped**.

## Distinctions that survive every stage

**GrokPtah already routes Grok Build session/gateway traffic** via
`~/.grok/auth.json` / OIDC (`auth_store.rs`, cli-chat-proxy headers). Compatible
Grok Build gateway requests consume **provider quota as a provider-side effect**.
GrokPtah does not synchronize a Grok Build account balance. The PR #352 local
host quota ledger is a **separate pending feature until merged**.
`LiveCredentialAttestation.certification_ready` is a stricter campaign gate
and is not recorded as passed.

A **named, secret-free provider-quota receipt** is a **mandatory 100%
live-provider exit** (stage 2): campaign/credential/route-bound evidence of
provider-side request/quota **consumption** and **exhaustion/429** behavior.
That receipt is distinct from GrokPtah’s local host ledger and from full
account-balance synchronization. Account-balance synchronization is **not
implemented and not a 100% requirement**. A live report that says quota was
“not observed” **fails** the stage 2 exit. Hermetic catalog `http_429` checks
and replay fixture `rate-limit-backoff-recovery` are not that receipt. The
live attestation seam
([`GROK_BUILD_LIVE_ATTESTATION.md`](GROK_BUILD_LIVE_ATTESTATION.md),
`live_attestation.rs` `attest_grok_build_oidc_with_min_validity`) is
secret-free by design; it does **not** itself record the quota receipt.

**Executable xAI credential order** (`auth_store.rs` `resolve_xai_credentials`):
`XAI_API_KEY`, then the OS keychain API key, then `GROKPTAH_TOKEN_COMMAND`,
then the Grok Build session from `~/.grok/auth.json`. The module comment in
that file is stale; this order is the executable source.

**Current semantic Computer Use deliberately avoids raw global mouse injection** (`pointer_fallback: false`;
`unsupported_pointer_fallback_never_reaches_backend`). **Foreground activation is not equivalent to non-disruptive isolated Computer Use.** The
current slice activates the selected target and rechecks the frontmost
application (`ActivateTarget`, `GPTTargetIsFocused`). Background-safe
semantic ([#287](https://github.com/chriscase/GrokPtah/issues/287)) is a later
tier. **Isolated visual Computer Use
([#288](https://github.com/chriscase/GrokPtah/issues/288)) is a mandatory
product exit**, never an Explicitly unsupported alternative on the path to
100%. It requires a genuinely isolated agent-owned app surface/cursor:
global pointer, keyboard, focus, clipboard, and unrelated apps remain
unaffected; takeover is out-of-band and preemptive. Raw **global** injection
may remain Explicitly unsupported.

**Grokbot** is epic/ADR language for always-available hosted agents
([#301](https://github.com/chriscase/GrokPtah/issues/301),
[`ADR-002-runtime-boundaries.md`](ADR-002-runtime-boundaries.md)). There is
no shipped binary named Grokbot. Shipped `ManagerSupervisor` is not hosted
Grokbot certification.

**On the `origin/main` roadmap base, configured remote bearers can approve and
promote within service scope.** Possession of any `--token` / `--client`
bearer there is operator-equivalent for the full `CONTROL_TOOLS` surface. The
dream candidate replaces that model with explicit tiers, but it is not a
shipped claim until the Stage 3 campaign passes and the result is integrated.

**Company-approved OpenAI-compatible gateway quota is not Grok Build quota.**
Compatible-profile requests consume that company’s provider quota. That is
not the Stage 2 Grok Build provider-quota receipt and not a Grok Build
account balance. Closed [#169](https://github.com/chriscase/GrokPtah/issues/169)
(named compatible profiles) is not the Stage 12 enterprise review-lane
certification.

## What “100%” means (measurable exit)

A 100% claim is allowed only when **all twelve stages below have met their
exits**, and all of the following are true:

1. Every matrix row that this program marks Supported or Experimental has
   implementation objects on `origin/main` plus the tests named in the matrix.
2. No Planned or Explicitly unsupported row is described as shipped in
   README or allowlisted docs.
3. No open P0 Computer Use issue
   ([#267](https://github.com/chriscase/GrokPtah/issues/267) children
   [#269](https://github.com/chriscase/GrokPtah/issues/269),
   [#270](https://github.com/chriscase/GrokPtah/issues/270),
   [#271](https://github.com/chriscase/GrokPtah/issues/271) mutations,
   [#274](https://github.com/chriscase/GrokPtah/issues/274),
   [#286](https://github.com/chriscase/GrokPtah/issues/286),
   [#287](https://github.com/chriscase/GrokPtah/issues/287),
   [#288](https://github.com/chriscase/GrokPtah/issues/288)) remains without
   a recorded close proof that matches its acceptance criteria. **[#288](https://github.com/chriscase/GrokPtah/issues/288)
   is on this mandatory proof list.** Isolated visual Computer Use cannot be
   waived as Explicitly unsupported.
4. A named live Grok Build campaign report is committed or attached to the
   closing issue, covering the catalog IDs in
   [`PERSISTENT_AGENT_CERTIFICATION.md`](PERSISTENT_AGENT_CERTIFICATION.md)
   that this roadmap requires, with `certification_ready == true` for that
   campaign’s credential binding. The same campaign record includes a
   **positive, named, secret-free provider-quota receipt**:
   campaign/credential/route-bound evidence of provider-side request/quota
   consumption **and** exhaustion/429 behavior. A statement that quota was
   “not observed” **fails** this item. The receipt is not a GrokPtah host
   ledger and is not full account-balance synchronization.
5. Least-privilege remote authority (stage 3) is shipped: `LocalOperator`,
   `RemoteCoordinator`, and `Observer` are separated; bearer authentication
   does not imply approve, promote, or Computer Use authority.
6. An always-on hosted **operational** soak report exists (duration ≥72 hours,
   restart count, zero implicit resumes, bounded resource growth) **and** is
   distinct from the long-horizon memory exit. Elapsed soak alone is
   insufficient. The **independent long-running worker / multi-worker**
   outcome ([#305](https://github.com/chriscase/GrokPtah/issues/305)) is
   proven: durable ownership, bounded delegated workloads, crash/restart
   recovery, no duplicate execution, capability/authority isolation, and
   retained evidence. That core product goal cannot be descoped, marked
   Explicitly unsupported, or otherwise status-relabeled away. Documented
   #305 non-goals may remain unsupported.
7. Long-horizon durable memory (stage 5) has accelerated logical-years
   evidence. Wall-clock soak is not a substitute.
8. Desktop and `grokptah-service` advertise a declared capability document
   and fail closed on missing host capabilities; hosted-service CI runs on
   `origin/main`. One versioned authenticated black-box fixture has compared
   public HTTP MCP against desktop loopback and standalone hosted service
   (stage 4).
9. Computer Use has an agent-owned interaction surface, a background-safe
   semantic tier **or** an explicit documented unsupported disposition with
   tests, **and a proven isolated visual backend satisfying [#288](https://github.com/chriscase/GrokPtah/issues/288)**.
   Raw global input remains Explicitly unsupported unless isolation is
   actually proven **without** global injection.
10. Packaged UX and accessibility certification for the Computer cockpit and
    the selected product UX direction ([#273](https://github.com/chriscase/GrokPtah/issues/273),
    [#308](https://github.com/chriscase/GrokPtah/issues/308)) is recorded: a
    **selected, documented UX direction** plus its **bounded packaged-desktop
    acceptance set** (keyboard/accessibility, operator workflows,
    wide/narrow/light/dark, reconnect/error/quota/authority states, visual
    evidence). Explicitly unsupported may cover **documented non-goals only**,
    not the Codex-class core interface. A **recurring expert UI/UX review
    cadence** (stage 10) supplements that one-time acceptance: it is not a
    single pre-release polish pass and cannot remain Unverified at 100%.
11. Operations and release drills have a dated runbook execution covering
    backup/restore, restart, cursor expiry, credential rotation, Computer Use
    Stop / Take over on a packaged identity, upgrade/rollback,
    disk-full/corrupt/torn-state recovery, sole-writer contention,
    monitoring/alerts, backup confidentiality, RTO/RPO, and the
    sccache / repository-family `CARGO_TARGET_DIR` ownership and cleanup
    policy in [`BUILD_PERFORMANCE.md`](BUILD_PERFORMANCE.md).
12. An **enterprise gateway review lane** is certified (stage 12): a user
    restricted to a company-approved OpenAI-compatible gateway, including a
    weaker non-frontier model, still obtains powerful long-running code
    review from bounded orchestration — not from secretly routing to a
    stronger external model. That outcome cannot be waived as Explicitly
    unsupported if we claim the full product vision.

Until every item holds, **do not claim 100%.**

## No-Unverified-at-100

The [Unverified](#unverified-explicit) list is the **current** 2026-08-24 gap
list. A trustworthy 100% claim is **invalid** if any of the following still
appear there, are omitted from recorded evidence, or are waived by descope /
Explicitly unsupported / “not observed” status-relabeling.

Candidate update (2026-08-24): draft PR #374 adds isolated-guest lifecycle,
lease, cleanup, and capture-redaction source proof in the review candidate;
its focused external tests do not qualify packaged VM hardware, guest boot,
rendered frames, host input, or soak. The candidate readiness UI now exposes
provider-route configuration, unsynchronized provider quota, and the boundary
between measured model evidence and a live campaign certificate. Neither
candidate-only change alters the `origin/main` claim or closes a live gate.

**This follow-up’s three exits — forbidden to remain Unverified at 100%:**

1. **Named secret-free provider-quota receipt** — campaign/credential/route-bound
   evidence of provider-side request/quota consumption **and** exhaustion/429
   behavior. Distinct from the GrokPtah local host ledger and from full
   account-balance synchronization. A report that says “not observed” fails.
2. **Independent long-running worker / multi-worker outcome**
   ([#305](https://github.com/chriscase/GrokPtah/issues/305) core) — durable
   ownership, bounded delegated workloads, crash/restart recovery, no
   duplicate execution, capability/authority isolation, retained evidence.
   Cannot be descoped. Documented non-goals may stay unsupported.
3. **Selected UX direction plus bounded packaged-desktop acceptance set**
   ([#308](https://github.com/chriscase/GrokPtah/issues/308) core) —
   keyboard/accessibility, operator workflows, wide/narrow/light/dark,
   reconnect/error/quota/authority states, visual evidence. Explicitly
   unsupported covers documented non-goals only, not the Codex-class core
   interface.

**Mandatory product goals added 2026-08-22 — also forbidden to remain
Unverified at 100%:**

4. **Recurring expert UI/UX review cadence** (stage 10 supplement, not a
   substitute for #308). Reviews the exact assembled integration head on a
   recorded cadence; unresolved P0/P1 UX/accessibility findings block the
   next integration/release gate. Phase 2 mockups and a one-time polish pass
   do not close this.
5. **Enterprise gateway long-running code-review lane** (stage 12). A frozen
   company-approved OpenAI-compatible route, including a modest non-frontier
   model, must deliver powerful multi-hour review from orchestration — not
   from secretly routing to a stronger external model. Closed [#169](https://github.com/chriscase/GrokPtah/issues/169)
   profiles are not this certification. Cannot be waived as Explicitly
   unsupported if we claim the full product vision.

**Already forbidden by earlier corrections (preserved):** isolated visual
Computer Use ([#288](https://github.com/chriscase/GrokPtah/issues/288)); named
live Grok Build campaign with `certification_ready == true`; least-privilege
tokens before any production-shaped soak; versioned black-box parity fixture;
logical-years memory evidence; 72-hour operational soak; packaged-identity
hardware matrix ([#274](https://github.com/chriscase/GrokPtah/issues/274)).

**May remain honestly absent at 100%:** full Grok Build account-balance
synchronization (not implemented, not required); raw **global** Computer Use
injection; documented #305 / #308 non-goals; Windows/Linux native Computer Use
until [#275](https://github.com/chriscase/GrokPtah/issues/275) /
[#276](https://github.com/chriscase/GrokPtah/issues/276) ship.

## Stage 1 — Merge-blocker repair

**Depends on:** nothing (current `origin/main`).

**Exists today:** Desktop CI
([`.github/workflows/desktop.yml`](../.github/workflows/desktop.yml)).
Native Coding RC and hosted-service CI live only on draft PR #352.
[#277](https://github.com/chriscase/GrokPtah/issues/277) (nanoid lockfile
audit) is **open**. Epic [#301](https://github.com/chriscase/GrokPtah/issues/301)
checkboxes are stale versus closed children.

**This stage cannot pass while [PR #352](https://github.com/chriscase/GrokPtah/pull/352)
remains draft.** Remaining draft is not an honest exit.

**Exit (all required):**

- This matrix/roadmap is on `origin/main` and allowlisted docs no longer
  contradict it, including [`TOOL_MATRIX.md`](TOOL_MATRIX.md).
- [#277](https://github.com/chriscase/GrokPtah/issues/277) is closed with
  `npm audit --json` reporting zero findings on the desktop lockfile, or an
  explicit documented residual with owner sign-off.
- Native Coding RC ([PR #352](https://github.com/chriscase/GrokPtah/pull/352))
  **or a superseding implementation** is **merged** only after the five
  independently confirmed P1s on inspected head
  `4bd2081b2945e8ce881895f976bb7c8d88b929f2` are **fixed and independently
  certified**:
  1. Typed public Run projection with **no provider-route leak** (public
     HTTP/MCP Run views must not expose route snapshots or secret-adjacent
     fields; readiness-only `projection_is_owner_scoped_and_omits_unrelated_provider_identity`
     is not sufficient).
  2. Immutable ManagerProposal **deny-all before** schema advertisement,
     event emission, and tool dispatch (`ToolGate::AutoDeny` / host-enforced
     proposal-only must be installed before those surfaces, including
     `bypassPermissions`).
  3. Frozen ManagerDecision **AgentSpec fence** (a captured
     `expectedAgentSpecRevision` cannot be widened by a later spec edit).
  4. `ProviderSendCertainty::UncertainAccept` **never auto-retries** on any
     admission path, including desktop and hosted reopen.
  5. Desktop admission **atomically** persists Run + quota reservation +
     Agent activation and **never dispatches** a provider call on failure
     (partial persist rolls back).
- Until that merge, every Native Coding Readiness Center, local quota ledger,
  and hosted-service.yml capability remains **Pending — not shipped**.
- Source drafts [#343](https://github.com/chriscase/GrokPtah/pull/343)–[#351](https://github.com/chriscase/GrokPtah/pull/351)
  are not described as shipped.
- Open issues [#305](https://github.com/chriscase/GrokPtah/issues/305) and
  [#308](https://github.com/chriscase/GrokPtah/issues/308) are not called
  complete.

**Must not claim:** Native Coding Readiness Center, local quota ledger, or
hosted-service.yml as shipped while they exist only on PR #352. **Must not
claim stage 1 pass** while PR #352 is still draft.

## Stage 2 — Live Grok Build certification

**Depends on:** stage 1 (no contradictory docs; Native Coding RC merged after
certified P1 repair).

**Exists today:** Supported OIDC/gateway **routing**; attestation module;
hermetic certification lab; catalog
[`evals/persistent-agent-scenarios.v1.json`](../evals/persistent-agent-scenarios.v1.json)
(`retry-transient-001` replay checks include `http_429`); hermetic replay
[`evals/certification-lab/replay-fixtures/provider-behaviors.v1.json`](../evals/certification-lab/replay-fixtures/provider-behaviors.v1.json)
(`rate-limit-backoff-recovery`). Those hermetic 429 fixtures are **not** a
live provider-quota receipt.
`certification_ready` stays false without an authoritatively verified client
policy ([`GROK_BUILD_LIVE_ATTESTATION.md`](GROK_BUILD_LIVE_ATTESTATION.md)).
The lab does not claim model quality or a passed live campaign
([`PERSISTENT_AGENT_CERTIFICATION_LAB.md`](PERSISTENT_AGENT_CERTIFICATION_LAB.md)).

**Exit (all required):**

- `attest_grok_build_oidc_with_min_validity` returns
  `certification_ready == true` for the campaign credential, with the
  positive schema only (no tokens in artifacts).
- Named catalog scenarios at least: `xai-route-oidc-001`, `sse-stream-001`,
  `native-tools-001`, `retry-transient-001`, `agent-initial-run-001`,
  `interrupt-recover-001`, `resume-idempotency-001`, `token-ceiling-001`
  have a live report that the lab classifies as a successful live run, not
  only hermetic replay.
- Provider observations carry the opaque credential binding only after
  attestation succeeds.
- A **positive, named, secret-free provider-quota receipt** is recorded for
  the same campaign: campaign/credential/route-bound evidence of
  **provider-side** request/quota **consumption** and **exhaustion/429**
  behavior. The artifact follows the live-attestation positive schema (no
  tokens, client identifiers, subjects, user/team identifiers, filesystem
  paths, arbitrary URLs, or provider response bodies).
- That receipt is **mandatory**. A live report that says Grok Build gateway
  quota was “not observed,” “not applicable,” or equivalent **fails this
  exit**.
- Hermetic `http_429` catalog checks and replay fixture
  `rate-limit-backoff-recovery` **do not** satisfy this exit.
- The receipt is **not** GrokPtah’s local host quota ledger (PR #352,
  pending until merged after certified P1 repair) and **not** full Grok Build
  account-balance synchronization. Account-balance synchronization remains
  **not implemented and not a 100% requirement**. Compatible gateway
  requests still consume provider quota as a provider-side effect. A merged
  local host ledger is still not a Grok Build balance.

**Must not claim:** “Grok Build certified” from hermetic replay or from
routing-only unit tests. **Must not claim** stage 2 pass from a report that
omits the named provider-quota receipt or states that quota was not observed.

## Stage 3 — Least-privilege remote authority

**Depends on:** stage 2 (live routing/tools proven on finite Runs). **Must
complete before any production-shaped 72-hour autonomous soak.**

**Exists today:** Operator-equivalent named bearers
(ADR-002 §5, [`HEADLESS_SERVICE.md`](HEADLESS_SERVICE.md)). **Every
configured remote bearer can directly approve and promote within service
scope** (`ptah_approve_run`, `ptah_promote_run`). Computer MCP **reads** are
on main; **mutations** remain [#271](https://github.com/chriscase/GrokPtah/issues/271)
**open**.

The dream candidate has a materially stronger implementation than
`origin/main`: hosted credentials are role-scoped as `RemoteOperator`,
`RemoteCoordinator`, or `Observer`; only the trusted local adapter can mint
`LocalOperator`; credential workspace/Agent grants can only narrow; worker
credentials are Agent-bound and rotatable; Computer-read grants are immutable
session/workspace capabilities; and the public capability document is derived
from the enforced operation set. The current dream candidate adds the
clean-exact-head `authority` campaign and independently sealed verifier. It
fixes seven ordered gate families at 22 tests and now explicitly tests that
host profiles are stable, role-separated, and hash-bound; a bearer cannot mint
local authority, Observer lacks the complete named mutation set, and Computer
reads deny cross-session/cross-workspace access indistinguishably.

This remains **implementation-complete but not certification-complete** in the
candidate. The new slice is formatted and statically reviewed, but no build or
test claim is made from this local sandbox: the mandatory external cached
target did not exist here, and the loopback gates require a host runner. Stage
3 exits only after a clean final head passes the campaign and its sealed report
passes independent inspection. Until that happens, do not begin a
production-shaped soak and do not describe `origin/main` as least-privilege.

**Exit (all required):**

- Each credential maps to a transport-neutral `AuthorityContext` (principal,
  credential id, tier, workspace/Agent scope, permitted operations).
- Tiers are separated at least as **LocalOperator**, **RemoteCoordinator**,
  and **Observer** (ADR-002 also names a bounded worker/client; that role may
  only narrow). Bearer authentication **must not** imply approve, promote, or
  Computer Use authority.
- At least one non-operator tier **cannot** call `ptah_approve_run`,
  `ptah_promote_run`, Computer Use mutation, or managed-execution enablement.
- Tests prove: wrong tier → typed forbidden; caller-supplied Agent ID is
  not authentication; Computer Use grants still require the local privileged
  operator on a capable host; Observer cannot mutate.
- [#271](https://github.com/chriscase/GrokPtah/issues/271) mutations stay
  disabled until this authority model and the threat review both pass.

**Must not claim on `origin/main`:** “scoped tokens” while every `--client`
bearer still receives `CONTROL_TOOLS` in full. **Must not start** a
production-shaped 72-hour soak on operator-equivalent bearers; the candidate
tier implementation must first pass the exact Stage 3 gate.

## Stage 4 — Desktop / hosted shared parity

**Depends on:** stage 3 (capability advertisement is unsafe while every
bearer is operator-equivalent).

**Exists on `origin/main`:** Shared runtime/protocol; Experimental parity; no
declared host capability document; hosted-service.yml **Pending — not
shipped**.

**Dream candidate:** Desktop and service entry paths now bind distinct stable
host assertions and capability sets into the versioned initialize document
without widening bearer authority. The existing public-MCP fixture verifies
attempt-time capture, restart stability, and typed denial of an undeclared
Computer mutation in addition to its readiness/quota/Manager/restart/redaction
oracle. Its current-head immutable golden and hosted qualification remain open,
so Stage 4 is **not** claimed complete.

**Exit (all required):**

- A versioned host-capability document is produced by both desktop and
  `grokptah-service` (stable IDs, host/version, attempt-time capture).
- Missing capabilities return typed unsupported/forbidden; they never fall
  back to broader filesystem or Computer Use access.
- `.github/workflows/hosted-service.yml` (or equivalent) runs on
  `origin/main` for `grokptah-service` fmt/clippy/test.
- **One versioned, authenticated black-box fixture** is committed and runs
  through **public HTTP MCP** against **desktop loopback** and **standalone
  hosted `grokptah-service`**, then compares **normalized** results for:
  readiness; native admission/quota; Manager proposal denial; restart
  dedup / `UncertainAccept`; redaction; and **exact cardinalities**. A crate
  sharing the same types is not this fixture.
- Conformance suite on a hosted-shaped config covers session create/list,
  submit/cancel, restart, cursor expiry, and Computer **read** authorization.
- Desktop remote client still cannot inherit Computer Use or keychain from
  the service.

**Must not claim:** “parity complete” from sharing a crate without declared
capabilities, hosted CI, and the black-box fixture above.

## Stage 5 — Long-horizon durable memory

**Depends on:** stages 1–4 (honest shipped memory on main, least-privilege
and host parity before treating hosted memory as production-shaped).

**Exists today:** Source-workspace memory with explicit `project` /
`agent_private` / `team` descriptors and fail-closed bounds
([`MEMORY_SCOPES.md`](MEMORY_SCOPES.md), `memory.rs`,
`tests/memory_scopes.rs`). Team scope is denied unless host policy approves an
ID. The dream candidate adds a v2 hot-store protocol with idempotent receipts,
revisions, compare-and-swap supersession, validity windows, conflict surfacing,
critical-fact-aware compaction, restart/cutpoint tests, and enforced
fact/byte/file/scope ceilings. Its deterministic accelerated campaign advances
ten logical years across all three scopes and measures critical recall, stale
current facts, conflict recall/false positives, duplicates, lexical retrieval,
reopen determinism, and storage bounds.

The candidate code slice `96c28cec36002785a8a03ca5d5d3dca1dbfa78f0`
also freezes Manager memory per occurrence. A deny-unknown attribution binds
the exact AgentSpec revision, canonical scope policy, source workspace, bounded
quoted project context, and decision objective; proposal-only execution
suppresses later ambient memory injection, and objective/spec/digest drift is
refused before provider admission or plan mutation.

One retained logical-years artifact exists at
[`docs/evidence/memory-long-horizon-campaign-v1.json`](evidence/memory-long-horizon-campaign-v1.json),
but it predates the Manager slice and deliberately remains
`claim_eligible: false`. Candidate `a530f20d59d64b1d9825690c45c553a1c4191852`
adds the integrated `memory` campaign runner documented in
[`PERSISTENT_AGENT_CERTIFICATION_LAB.md`](PERSISTENT_AGENT_CERTIFICATION_LAB.md).
It requires a clean exact head, runs ten ordered gates with exact green
cardinalities, binds fresh logical-years evidence to that same SHA, retains
only bounded digests, rechecks head/cleanliness, and seals only after every
gate passes. Tampered, incomplete, reordered, drifted, or false-claim evidence
fails independent inspection.

Stage 5 is therefore **implementation-complete in the candidate but not yet
certification-complete**. A local fail-closed exercise created only an
incomplete ignored campaign and no report/completion seal; it does not count
as evidence. The current integrated head still needs one retained passing
exact-head campaign on a host/CI runner that permits the required supervisor
and native loopback gates. Run it serially with the explicit sccache and
external-target environment in the lab guide; an in-checkout target is not an
acceptable campaign setup.

**Exit (all required):**

- Accelerated **logical-years** retention evidence (revision, supersession,
  expiry, conflict) with named fixtures; not elapsed wall-clock.
- Scope isolation remains exact across project / agent-private / team after
  compaction and retrieval.
- Compaction and retrieval quality are measured against committed fixtures
  (precision/recall or an equivalent named metric).
- Manager frozen memory attribution: a ManagerDecision cannot silently
  rewrite or widen memory that was captured under an earlier AgentSpec /
  occurrence fence.
- Restart and crash consistency: no lost, duplicated, or cross-scope facts;
  torn writes fail closed.
- Storage bounds are enforced and tested (facts, bytes, files, compaction
  budget).
- This exit is **separate** from the 72-hour operational soak. Elapsed soak
  alone does not certify years-long agents.

**Must not claim:** years-long memory from the current 80-fact file store or
from a 72-hour uptime report.

## Stage 6 — Always-on Grokbot certification and 72-hour operational soak

**Depends on:** stages 3–5 (least-privilege tokens, parity fixture, and
memory contract exist before production-shaped autonomy).

**Exists today:** Experimental manager supervisor + hosted home; native
executor; routines (manual/schedule); first transport-neutral workload and
coordinator/worker slices
([`DURABLE_WORKLOADS.md`](DURABLE_WORKLOADS.md),
[`COORDINATOR_WORKERS.md`](COORDINATOR_WORKERS.md);
`tests/coordinator_mcp.rs` `independent_worker_recovers_assignment_and_messages`).
[#307](https://github.com/chriscase/GrokPtah/issues/307) is **closed**.
[#305](https://github.com/chriscase/GrokPtah/issues/305) remains **open**.
Those first slices are **not** the independent long-running multi-worker 100%
exit. Grokbot is not a binary. Unattended Computer Use is Explicitly
unsupported. Certification-lab smoke checks that managed execution is
**disabled by default**. No 72-hour soak report exists. The v51 public-run
campaign is **NOT QUALIFIED**; the exact v52 repair/certification handoff is
[`ALWAYS_ON_GROKBOT_V52_HANDOFF.md`](ALWAYS_ON_GROKBOT_V52_HANDOFF.md). The
dream candidate now accepts externally managed, Agent-bound worker credentials,
scopes them to
the final service workspace allowlist, rejects token reuse, and has a real
service-process harness that holds two independent worker leases across the
manager crash fence, rotates both credentials, rejects both retired bearers,
and emits the v2 secret-free evidence record only after a clean-head 72-hour
run. That harness is formatted but uncompiled and unexecuted; it is not a
retained campaign. Unbound remote bearers remain coordinator-scoped.

The packaged lease-fence follow-up is now running as an external, fail-closed
source qualification from immutable bundle
`/private/tmp/grokptah-packaged-lease-b250b70-v2.bundle`
(SHA-256 `4d4f46a85168b45476c1acc47ba7e289bfcb27b6ea08b173d862a038f27a2352`).
No packaged VM, guest boot, rendered-frame, host-input, signing, or soak claim
may be inferred until that report and the later hardware campaign return.

**Exit (all required):**

- A hosted `grokptah-service` instance remains the sole writer of one
  `GROKPTAH_HOME` for a declared soak window of at least **72 hours**, with
  at least **three** process restarts, using **least-privilege** credentials
  (Observer cannot approve/promote; RemoteCoordinator cannot gain
  LocalOperator Computer Use).
- Soak log shows: zero implicit model resumes; interrupted Runs stay
  `interrupted`; autonomous manager plans (if enabled) stay within
  documented bounds (≤64 steps, ≤16 in-flight, ≤16 replans); `/ready` fails
  closed on persistence errors.
- Native managed execution, when enabled for the soak, still rejects
  Computer Use and `bypassPermissions`.
- [#301](https://github.com/chriscase/GrokPtah/issues/301) is closed only
  when its remaining **open** children that this stage owns are closed;
  stale epic checkboxes are not evidence.
- [#305](https://github.com/chriscase/GrokPtah/issues/305) is **closed with
  independent-worker / multi-worker proof**. Descope, Explicitly unsupported,
  and other status-relabeling of that core product goal **fail this exit**.
  Required, retained evidence:
  - **durable ownership** (WorkItem / WorkAttempt / Agent identity;
    lease-token scoped claims; lane archival does not mutate workload state);
  - **bounded delegated workloads** (parent/child Work, assignment states,
    native-executor bounds; Computer Use / `bypassPermissions` still
    rejected on this path);
  - **crash/restart recovery** (store reopen preserves workers, decisions,
    messages, Work, leases, attempts, progress, and terminal results);
  - **no duplicate execution** (two workers cannot both hold a valid lease;
    request-id idempotency; expired leases do not complete on a stale token);
  - **capability/authority isolation** (a worker cannot widen bounds or
    Computer Use the manager does not possess; caller-supplied Agent IDs are
    not authentication; the runtime's bound-worker credential contract and
    its production issuance/rotation from stage 3 apply);
  - **retained evidence** (attempt history, artifacts, ordered events
    suitable for cursor replay and audit).
  The secret-free evidence shape is defined in
  [`WORKER_CERTIFICATION_EVIDENCE.md`](WORKER_CERTIFICATION_EVIDENCE.md) and
  enforced by `worker_certification_evidence.rs`; it does not substitute for
  the dated production-shaped campaign.
  Candidate runner: `tests/always_on_grokbot.rs`
  `certify_stage6_multi_worker_72h`. It rejects a duration override other than
  259200 seconds, requires a clean unchanged HEAD, keeps one service-owned
  home, and writes the secret-free report outside the repository.
  Documented #305 **non-goals** may remain Explicitly unsupported: scheduler
  or webhook adapters, model-based prioritizer, automatic permission
  approval/promotion, distributed consensus or multi-node scheduler, public
  multi-tenant queue, and a second storage subsystem. Message-triggered
  `RoutineTrigger::External` may stay unsupported. The independent
  long-running agent outcome itself cannot.
- First-slice objects (`independent_worker_recovers_assignment_and_messages`,
  closed [#307](https://github.com/chriscase/GrokPtah/issues/307)) do **not**
  close this exit while [#305](https://github.com/chriscase/GrokPtah/issues/305)
  remains open.
- This soak is **operational**. It does not replace stage 5 memory evidence
  and does not replace the independent-worker proof above.

**Must not claim:** always-on Grokbot, unattended Computer Use, or soak
from desktop-focused sessions only. **Must not claim** years-long agents
from this soak. **Must not claim** the independent long-running worker
outcome from the first workload/coordinator slice or by descoping #305.

## Stage 7 — Agent-owned Computer Use surface

**Depends on:** stages 1–2 for provider honesty; does not require stage 6
for a local-only slice, but **release** of the surface as 100% does.

**Exists today:** Experimental foreground semantic CU + cockpit projection
([`computerActivity.ts`](../desktop/src/lib/computerActivity.ts)). The stacked
coordination candidate adds a local-only, secret-free queue/owner explanation
for the durable WorkAttempt surface ledger. Its stacked emergency-control
successor keeps Pause / Stop / Take over outside the ordinary UI busy gate,
uses current host state rather than a client-held version, and provides stable
keyboard paths. The app-owned successor binds those controls to the exact Run
outside cockpit visibility. Its typed-replay successor adds a closed
redaction-safe event vocabulary, exact session/Run cursor persistence, and a
sticky visible history-gap state that does not block emergency controls. These
are candidate objects pending external Rust qualification and integration. The
stacked agent-attention successor adds redaction-safe proposal/attention/
approval/rejection events plus an accessible app-owned marker bound to the exact
current observation. It uses normalized in-surface coordinates, never moves or
impersonates the operating-system pointer, emits no marker for a manual proposal,
and refuses to invent a position for missing/out-of-surface geometry. It is still a
candidate pending external Rust and visual qualification. It does **not** add a
background-safe backend or an isolated visual input domain. Its stacked native-
cancellation successor closes the action-mutex inversion with an exact Run-scoped,
per-action atomic signal and native checkpoints before Accessibility dispatch and throughout
the bounded activation wait. This is out-of-band preemption of work that has not
entered an atomic AX call; an AX call already inside macOS remains uncertain and
non-replayable. External Rust/native qualification and the remaining #286 packaged
takeover evidence are still required.
[#286](https://github.com/chriscase/GrokPtah/issues/286) **open**.

**Exit:** every [#286](https://github.com/chriscase/GrokPtah/issues/286)
acceptance criterion, including: user pointer unchanged; agent cursor only
inside the authorized surface; persistent Stop / Take over; takeover is
out-of-band and preemptive; no raw pointer fallback introduced by the
surface layer.

**Must not claim:** Codex-like Computer Use from the current cockpit
preview.

## Stage 8 — Background-safe semantic execution

**Depends on:** stage 7 (activity/attention events without OS-pointer
takeover).

**Exists today:** The shipped/base native path requires foreground activation
(`GPTTargetIsFocused`). A stacked source-qualified candidate adds one narrow
measured-background path: reversible disposable-target calibration and a
short-lived, one-use, exact target/process/window/element receipt for visible
text entry only. Native dispatch measures the foreground process, active
window, and physical pointer before/after, rechecks that the target remains
background, and denies activation/raw fallback. The cockpit visibly separates
the two modes. Deterministic source/UI tests pass; strict external Rust/native
and packaged disposable-fixture evidence are pending. Invoke/select/scroll and
unsupported-target dispositions remain open. [#287](https://github.com/chriscase/GrokPtah/issues/287)
**open**; this candidate does not meet the full stage exit.

**Exit:** every [#287](https://github.com/chriscase/GrokPtah/issues/287)
acceptance criterion, including: a supported background action leaves
foreground app, active window, and physical pointer unchanged; unsupported
targets require explicit foreground authorization; no silent raw-input
fallback. A documented Explicitly unsupported disposition with tests is
allowed **for this background-safe tier only**.

**Must not claim:** background-safe CU because Accessibility `invoke`
exists. Foreground activation is not this stage.

## Stage 9 — Isolated visual backend (mandatory product exit)

**Depends on:** stage 7 (agent-owned pointer contract). Stage 8 is not a
substitute.

**Exists today:** `ComputerUseTier::VisualFallbackAct` is not granted by
the first probe. [#288](https://github.com/chriscase/GrokPtah/issues/288)
**open**. Hidden windows, Spaces, and global `CGEvent` injection **do not
qualify**. The source-only Stage 9 investigation now selects an authenticated,
networkless-by-default disposable VM as the arbitrary-GUI boundary and rejects
hidden windows, Spaces, live coordinate injection, the simulator, and a WebView
alone as sufficient proof. The Stage 9 candidate also has an additive guest-only pointer
move/button/text contract and a read-only macOS probe for the minimum OS, required Virtualization
classes. The probe explicitly reports the packaged helper entitlement/image as pending, keeps the
main process unprivileged, and never launches a VM or grants authority. A further candidate contract
freezes the initial
network/clipboard/share/credential/input-device-free profile, resource ceilings, exact packaged
digests, restart interruption, and cleanup-before-terminal lifecycle without serializing host paths
or channel secrets. A subsequent read-only protocol candidate HMAC-binds the exact Run/surface
incarnation, payload length, one outstanding request nonce, monotonic message/frame sequences, and
zero input sequence for a closed observe/frame-metadata/health/failure/stop/shutdown-ack vocabulary.
An open-handle measurement candidate separately streams helper/image/configuration content through
bounded SHA-256 measurements, rejects writable descriptors and unsafe modes, detects file identity
changes, and emits no paths or descriptors. A later source candidate adds fixed exact bundle paths,
no-follow retained handles, strict whole-app/nested-code and helper signature validation, matching
team/identifier checks, a canonical designated-requirement digest, and the helper-only App Sandbox +
virtualization entitlement boundary. It rejects VM networking, debug attachment, unreviewed helper
entitlements, path replacement, and content/manifest drift. No helper, guest, configuration, signing
pipeline, or entitlement file is actually added by that verifier slice; it has no packaged runtime
evidence. A following source slice adds the minimal helper, exact entitlement/configuration files,
and a credentialed nested-signing assembler. The helper's closed inherited-descriptor bootstrap can
   configure a one-display/virtio-socket VM with no network/share/audio/storage/host-input devices,
   challenge/response guest bootstrap, authenticated shutdown acknowledgement, and bounded
   graceful/forced stop, but there is no reviewed guest image, signing identity, assembled app,
   signed-package launch, or cleanup run. The protocol does not implement a carrier, transfer frame
   bytes, render a frame, or launch a guest application. The repository now also carries a pinned Linux arm64
   guest-source lock, closed kernel fragment, freestanding guest PID 1, protocol self-test, and a
   Linux-only deterministic image-builder candidate. A dedicated Linux workflow builds that source
   twice and compares image/manifest bytes; it deliberately does not embed or publish the output.
   The Rust bridge and freestanding guest C now also share a length-prefixed
   Run/surface/incarnation/input-domain binding digest, challenge-derived channel key, and
   confirmation tag. The Rust frame/input carriers can derive interoperable keys from that
   challenge, and the helper/guest source loop now consumes the binding packet and returns an
   authenticated acknowledgement. The helper source defines a private control-channel relay and
   validates the guest acknowledgement. The candidate now also contains a macOS packaged-supervisor
   source seam that revalidates signed artifact handles, spawns only the allowlisted helper
   descriptors with close-on-exec, consumes the private challenge, and owns bounded lifecycle
   cleanup. These are contracts and unshipped source/pipeline primitives, not packaged evidence. A source-only
   `IsolatedVisualRuntimeSession` now couples helper event order to lifecycle cleanup, frame
   freshness, and challenge-bound input admission; it does not spawn or dispatch a packaged
   runtime. A bounded length-delimited `IsolatedVisualStream` supplies the private transport seam
   while still opening no VSOCK and dispatching no packaged runtime. `IsolatedVisualHelperControl`
   binds inherited helper control/event descriptors to the coordinator without spawning a process.
   The freestanding guest now validates the authenticated length-bounded input ABI after binding,
   captures a fixed `/dev/fb0` surface, renders a deterministic fixture, and emits authenticated
   bounded frame chunks; the helper relays those chunks over the private FD8 channel and returns
   its per-launch challenge over private FD9. Input still refuses admission until a reviewed
   packaged capture establishes a freshness fence. Its source identity and safe-check record are captured in
   [`COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md`](COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md). See
[`COMPUTER_USE_ISOLATED_VISUAL.md`](COMPUTER_USE_ISOLATED_VISUAL.md). The host-supervisor source
seam is not wired into capability admission and has not been run from a signed package. No VM boot,
signed/built helper, reviewed guest image, packaged entitlement proof, rendered frame, input,
cleanup, or host-native dispatch evidence exists yet, so these source slices satisfy no #288
acceptance checkbox. The credentialed execution and evidence handoff is defined in
[`COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md`](COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md).
The ordinary `grokptah.computer-qualification.v1` record remains semantic-only; visual fallback
authority additionally requires a **measured** record with the separate
`grokptah.isolated-visual-computer-qualification.v1` schema, which cannot be written before the
packaged campaign and independent review pass.

**Exit:** every [#288](https://github.com/chriscase/GrokPtah/issues/288)
acceptance criterion. **This row cannot be marked Explicitly unsupported as
a path to 100%.** Required isolation:

- a genuinely isolated **agent-owned app surface/cursor**;
- global pointer, keyboard, focus, clipboard, and unrelated apps remain
  unaffected;
- takeover is **out-of-band and preemptive**;
- raw **global** injection remains Explicitly unsupported.

**Must not claim:** isolated visual CU from screenshots of the live desktop,
foreground `ActivateTarget`, hidden windows, or Spaces.

## Stage 10 — Packaged UX and accessibility certification

**Depends on:** stages 7–9 for Computer Use UX; stage 4 for hosted/desktop
shared language.

**Exists today:** UX audit artifacts under `docs/ux-audit/` and
`docs/ux-design/`. Phase 2 comparison
([`ux-design/phase-2/comparison-and-recommendation.md`](ux-design/phase-2/comparison-and-recommendation.md))
recommends Direction 1 Focused Lane Workbench as the first-ship bet
(scores D1 88.75 / D2 86.25 / D3 77.50) composed with D2’s Agents spine and
D3’s opt-in supervision. That package is **not** a packaged-desktop
acceptance set and does **not** by itself select the 100% direction.
[#273](https://github.com/chriscase/GrokPtah/issues/273)
and [#308](https://github.com/chriscase/GrokPtah/issues/308) **open**.
[#274](https://github.com/chriscase/GrokPtah/issues/274) packaged-identity
proof still required.

**Exit (all required):**

- Packaged GrokPtah identity (stable Developer ID + bundle id) completes
  the three-action disposable macOS fixture with Screen Recording and
  Accessibility grants ([`COMPUTER_USE_MACOS.md`](COMPUTER_USE_MACOS.md),
  [#274](https://github.com/chriscase/GrokPtah/issues/274)).
- [#273](https://github.com/chriscase/GrokPtah/issues/273) a11y criteria
  (keyboard, names, focus return, reduced motion, narrow layout) pass on
  that packaged build.
- [#308](https://github.com/chriscase/GrokPtah/issues/308) has a **selected,
  documented UX direction** (Agents vs Lanes vs finite Run language matches
  ADR-002) and a **bounded packaged-desktop acceptance set** executed on
  that packaged identity, covering at least:
  - keyboard and accessibility;
  - operator workflows (first launch/provider setup, local project or hosted
    home, steer a coding lane, inspect tools/terminal/diffs/tests/evidence,
    review/approve/promote or discard, recover from errors/disconnects/
    restarts/interrupted Runs);
  - wide / narrow / light / dark;
  - reconnect, error, quota, and authority states;
  - visual evidence (before/after captures of the packaged build, not only
    the Phase 2 HTML prototype).
- Marking remaining **core** UX gaps Explicitly unsupported **fails this
  exit**. Explicitly unsupported may cover documented #308 **non-goals
  only**: no immediate full rewrite, no design chosen solely from a static
  beauty shot, no removal of advanced functionality without workflow
  evidence, no forced desktop/mobile layout parity, and no dependency on one
  model or design tool. Web/mobile clients need not share the desktop
  layout. The Codex-class core desktop interface cannot be status-relabeled
  away.
- The Phase 2 design package and D1 recommendation are inputs. They do not
  satisfy this exit until a direction is selected in the 100% record and the
  packaged-desktop acceptance set passes.
- **Recurring expert UI/UX review cadence** (mandatory supplement; not a
  substitute for the #308 selected direction and packaged acceptance; not
  one pre-release polish pass). GrokPtah must be periodically reviewed by a
  skilled UI/UX expert so it remains sleek, aesthetically coherent,
  accessible, approachable, and exceptionally effective for power users.
  - During active development: an expert review after each material
    operator-surface integration wave **or** every 2–3 significant GUI
    changes, whichever comes first, **plus** a full packaged-desktop review
    before release.
  - Review the **exact assembled integration head**, not mockups alone.
    Record SHA, reviewer/model/tool, surfaces/workflows, visual evidence,
    severity-ranked findings, accepted tradeoffs, and issue/PR follow-ups.
  - Cover progressive disclosure, information density, navigation/search,
    command/keyboard efficiency, bulk/multi-lane workflows, status/evidence
    clarity, error/reconnect/quota/authority/permission states, and
    preservation of advanced functionality.
  - Accessibility: full keyboard use, focus order/visibility, screen-reader
    labels/status, contrast, zoom/reflow, reduced motion, platform
    conventions.
  - Visual matrix: wide/narrow, light/dark,
    empty/loading/success/error/denied/exhausted/reconnecting, hostile/long
    text and overflow, real packaged windows.
  - Unresolved P0/P1 UX/accessibility findings **block** the next
    integration/release gate. Lower findings require explicit disposition
    and regression coverage.
  - The secret-free evidence shape is defined in
    [`UI_REVIEW_EVIDENCE.md`](UI_REVIEW_EVIDENCE.md) and enforced by
    `ui_review_evidence.rs`; it does not replace the dated expert review.
  No dated cadence review of an assembled head is recorded. Phase 2
  prototypes do not close this.

**Must not claim:** packaged UX certified from `tauri:dev`, terminal-owned
TCC grants, or prototype screenshots alone. **Must not claim** the expert
cadence from a single audit or from mockups of an unintegrated head.

## Stage 11 — Operations and release drills

**Depends on:** stages 2–6 and 10 (there must be something real to operate,
including least-privilege soak).

**Exists today:** Documented backup/restore and `/ready` behavior
([`HEADLESS_SERVICE.md`](HEADLESS_SERVICE.md)); deterministic service
conformance; Computer Use Stop / Take over tests in-process; authoritative
local sccache / repository-family target policy in
[`BUILD_PERFORMANCE.md`](BUILD_PERFORMANCE.md). No dated production-like
drill report, including no recorded sccache/target drill evidence.

**Exit (all required):**

- A written runbook is executed at least once on a disposable packaged
  desktop **and** a disposable hosted service: stop, copy `GROKPTAH_HOME`,
  restore to one writer, verify `/ready`, inspect interrupted Runs, explicit
  resume only.
- **Upgrade and rollback** of a named release artifact, with durable-home
  schema compatibility recorded.
- **Disk-full, corrupt, and torn-state** recovery: `/ready` fails closed;
  no silent journal truncation presented as complete history.
- **Sole-writer contention:** a second process cannot become a concurrent
  writer; lock failure is explicit.
- **Monitoring and alerts** for persistence errors, soak bound violations,
  and failed `/ready`.
- **Backup confidentiality:** `GROKPTAH_HOME` copies exclude live bearer
  tokens; restored backups do not leak credentials into logs or MCP.
- Documented **RTO/RPO** for the hosted home, measured on the drill.
- **sccache / repository-family target ownership and cleanup** (authoritative
  policy in [`BUILD_PERFORMANCE.md`](BUILD_PERFORMANCE.md); per-worktree
  private targets and optional `sccache` are **not** this exit):
  - After verifying `sccache` on PATH, local operator builds set
    `RUSTC_WRAPPER=sccache` with the stable shared cache
    `~/Library/Caches/grokptah/sccache`.
  - Compatible, **non-concurrent** lanes reuse one stable
    repository-family `CARGO_TARGET_DIR` **outside checkouts**, keyed and
    fenced by compatible toolchain, rustc target, features/profile, and
    lock/dependency graph.
  - Never concurrently share a writable target. Truly concurrent or
    incompatible builds get an exact isolated target **only for that lane**,
    then it is removed when inactive.
  - Never put multi-GB targets under `/private/tmp` or per review/worktree
    by default.
  - Before cleanup: exact target path and size, owner, no `cargo`/`rustc`
    process, no open handles; refuse deletion of active, protected, or
    shared-family paths. Build artifacts are disposable; source and commits
    are deliverables.
  - Handoffs record target and sccache paths, owner, and reason. Drill
    evidence covers compatible sequential reuse, concurrent forced
    isolation, incompatible forced isolation, crash cleanup, and
    active-target deletion refusal.
- Credential rotation: API-key, `GROKPTAH_TOKEN_COMMAND`, and OIDC-principal
  change invalidate measured qualifications as documented; ordinary
  access-token refresh does not.
- Computer Use drill: Pause, Stop, Take over (out-of-band, preemptive),
  permission revocation on the packaged identity.
- Cursor-expiry and MCP reconnect drill matches `service_conformance.rs`
  guarantees.
- Release artifact (notarization is still a documented non-goal unless this
  stage explicitly adds it) is identified by version, not by a commit SHA
  treated as an immortal product fact.

**Must not claim:** operations certified from unit tests alone.

## Stage 12 — Enterprise gateway review lane

**Depends on:** stages 3–6 (least-privilege, parity, memory, independent
workers) and stage 10 (operator workflow language). Provider-neutral
OpenAI-compatible **profiles** already exist on main
([#169](https://github.com/chriscase/GrokPtah/issues/169) closed); they do
**not** certify this lane.

**Exists today:** Named OpenAI-compatible profiles and measured coding-ready
vs discussion-only qualification
([`PROVIDER_PROFILES.md`](PROVIDER_PROFILES.md)). Isolated Build worktrees
and finite Runs exist as separate contracts. The certification lab now has a
host-only, secret-free lease attachment path for the operator-owned broker;
the candidate verifies a detached Ed25519 gateway signature against a separate
operator-selected public trust record before admitting that lease. That path
still does not execute a provider campaign. The candidate also exposes
`ptah_create_enterprise_review`, which verifies that signed handoff and
materializes the seven bounded passes into durable WorkItems with stable
idempotency keys. Each pass carries the signed provider/model/endpoint/
credential constraint in its durable WorkPolicy, and native execution compares
that constraint with the exact provider snapshot before creating a Run;
offline execution and every route or credential drift fail closed. The same
WorkPolicy carries a `read-only` sandbox ceiling for enterprise passes; both
interactive assignment and managed execution reject a worker whose captured
sandbox is broader. **No**
live frozen-route, read-only, multi-hour enterprise
review-lane certification exists. This row is **not** certified.

The executable external procedure is
[`ENTERPRISE_REVIEW_V1_HANDOFF.md`](ENTERPRISE_REVIEW_V1_HANDOFF.md). It pins
the candidate source, cache/target ownership, broker lease boundary, held-out
24-case paired campaign, denial matrix, quality thresholds, secret-free
evidence, cleanup, and independent-review requirements. It is deliberately a
handoff and does not upgrade this row until a real named gateway campaign
passes.

**Exit (all required):**

- A user restricted to a **company-approved OpenAI-compatible gateway**,
  even with a weaker non-frontier model, still obtains powerful
  **long-running code review**. Orchestrator strength comes from bounded
  decomposition; specialized passes (correctness, security, concurrency,
  performance, tests, API, UX as relevant); deterministic static/tool
  evidence; disagreement/adversarial checks; durable memory/checkpoints;
  and evidence-grounded synthesis — **not** from secretly routing to a
  stronger external model.
- The exact approved provider route is **frozen and auditable**. Code,
  prompts, and artifacts never leave the configured company boundary.
  **No fallback** to another provider (including Grok Build / xAI).
  Capability, credential, or route changes **fail closed** and require
  requalification.
- **Read-only by default:** isolated checkout, explicit file/repo scope,
  no builds, network, writes, or PR comments unless separately authorized.
  Publishing comments or fixes is a **distinct** least-privilege capability
  and approval (stage 3).
- Output cites exact code locations/evidence, distinguishes confirmed
  findings from hypotheses, shows confidence and model limitations,
  deduplicates across passes, and retains a **secret-free** audit trail.
- **Certification (live, named, unmet):** a deliberately modest compatible
  model completes a seeded multi-hour review corpus with measured
  recall/precision, bounded cost/time/retries, restart continuity, **zero**
  code/secret egress, **no** mutation, and useful findings materially
  better than a single-pass baseline. Also certify **denial** on route
  drift, missing capability, quota exhaustion, and unauthorized publish.
- Integrate with provider-neutral execution, long-running workers
  ([#305](https://github.com/chriscase/GrokPtah/issues/305)), least
  privilege, memory, desktop/hosted parity, and the Stage 10 operator
  workflow. Company-gateway quota is **that provider’s** quota; it is not
  the Stage 2 Grok Build provider-quota receipt and not a Grok Build
  account balance.
- Marking this core product outcome Explicitly unsupported **fails** a
  full-product 100% claim. Documented non-goals may stay unsupported
  (proprietary non-OpenAI-compatible APIs remain out of scope until ADR-002
  §6 evidence).

**Must not claim:** enterprise review-lane certification from closed #169
profiles, from Grok Build routing, from hermetic replay, or from a
single-pass chat on a frontier model.

## Dependency graph (summary)

```text
1 merge-blocker repair (PR #352 must merge after certified P1s)
        │
        ▼
2 live Grok Build certification
        │
        ▼
3 least-privilege remote authority
        │
        ▼
4 desktop/hosted shared parity (black-box fixture)
        │
        ▼
5 long-horizon durable memory
        │
        ▼
6 72-hour operational soak + independent-worker/#305 (cannot descope)
        │
        ├──────────────► 7 agent-owned Computer Use surface
        │                         │
        │              ┌──────────┴──────────┐
        │              ▼                     ▼
        │     8 background-safe      9 isolated visual (mandatory)
        │              │                     │
        └──────────────┴──────────┬──────────┘
                                  ▼
                 10 packaged UX + #308 acceptance + recurring expert cadence
                                  │
                    ┌─────────────┴──────────────┐
                    ▼                            ▼
     11 operations and release drills     12 enterprise gateway review lane
                    │                            │
                    └─────────────┬──────────────┘
                                  ▼
                         trustworthy 100% claim
```

Stages 7–9 may be implemented locally after stage 2, but they **do not
count toward 100%** until stages 3–6 and 10–12 are also done. Isolated
visual (stage 9) has no Explicitly unsupported waiver. Independent
long-running workers (stage 6 / #305) have no descope waiver. Packaged
core UX (stage 10 / #308) has no Explicitly unsupported waiver for the
Codex-class interface. Recurring expert UX cadence (stage 10) has no
one-polish-pass waiver. The enterprise gateway review lane (stage 12)
has no Explicitly unsupported waiver if we claim the full product vision.

## Unverified (explicit)

The following remain **unverified** as of 2026-08-24. They are not
shipped facts. Items marked **must not remain Unverified at 100%** are
listed in [No-Unverified-at-100](#no-unverified-at-100); a 100% claim that
still carries them here is invalid.

- Any live Grok Build campaign result (`certification_ready`, catalog IDs
  above). **Must not remain Unverified at 100%.**
- Named secret-free **provider-quota receipt** (campaign/credential/route-bound
  consumption and exhaustion/429). **Must not remain Unverified at 100%.**
  Hermetic `http_429` / `rate-limit-backoff-recovery` do not close this.
- Full Grok Build **account-balance API / synchronization** — not implemented
  and **not** a 100% requirement. May remain absent.
- GrokPtah **local host quota ledger** until PR #352 (or successor) merges
  after certified P1 repair.
- Packaged-identity Computer Use hardware matrix ([#274](https://github.com/chriscase/GrokPtah/issues/274)).
  **Must not remain Unverified at 100%.**
- Isolated visual Computer Use ([#288](https://github.com/chriscase/GrokPtah/issues/288)).
  **Must not remain Unverified at 100%.**
- Always-on 72-hour operational soak. **Must not remain Unverified at 100%.**
- Independent long-running worker / multi-worker outcome
  ([#305](https://github.com/chriscase/GrokPtah/issues/305) core).
  **Must not remain Unverified at 100%.** Cannot be closed by descope.
- Long-horizon / logical-years memory evidence. **Must not remain Unverified at 100%.**
- Least-privilege tokens in production-shaped configs. **Must not remain Unverified at 100%.**
- Versioned desktop-loopback vs hosted black-box parity fixture.
  **Must not remain Unverified at 100%.**
- Native Coding Readiness Center / hosted-service CI until those objects
  exist on `origin/main` after certified P1 repair.
- Selected UX direction plus bounded packaged-desktop acceptance set
  ([#308](https://github.com/chriscase/GrokPtah/issues/308) core).
  **Must not remain Unverified at 100%.** Cannot be closed by marking the
  Codex-class core interface Explicitly unsupported.
- Recurring expert UI/UX review cadence (stage 10 supplement).
  **Must not remain Unverified at 100%.** Cannot be closed by mockups or a
  single pre-release polish pass.
- Enterprise gateway long-running code-review lane (stage 12).
  **Must not remain Unverified at 100%.** Closed [#169](https://github.com/chriscase/GrokPtah/issues/169)
  profiles and Grok Build routing do not close this. Cannot be waived as
  Explicitly unsupported if we claim the full product vision.

See [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) for per-row evidence.
