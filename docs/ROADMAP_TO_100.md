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
| **Live-provider** | Named Grok Build campaign with `certification_ready == true` and catalog IDs | Host quota ledger, account-balance sync (not implemented), isolated visual Computer Use |
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

**Current configured remote bearers can approve and promote within service
scope.** Possession of any `--token` / `--client` bearer is operator-equivalent
for the full `CONTROL_TOOLS` surface, including `ptah_approve_run` and
`ptah_promote_run`. Bearer authentication must not imply that authority once
least-privilege ships.

## What “100%” means (measurable exit)

A 100% claim is allowed only when **all eleven stages below have met their
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
   campaign’s credential binding.
5. Least-privilege remote authority (stage 3) is shipped: `LocalOperator`,
   `RemoteCoordinator`, and `Observer` are separated; bearer authentication
   does not imply approve, promote, or Computer Use authority.
6. An always-on hosted **operational** soak report exists (duration ≥72 hours,
   restart count, zero implicit resumes, bounded resource growth) **and** is
   distinct from the long-horizon memory exit. Elapsed soak alone is
   insufficient.
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
    [#308](https://github.com/chriscase/GrokPtah/issues/308)) is recorded.
11. Operations and release drills have a dated runbook execution covering
    backup/restore, restart, cursor expiry, credential rotation, Computer Use
    Stop / Take over on a packaged identity, upgrade/rollback,
    disk-full/corrupt/torn-state recovery, sole-writer contention,
    monitoring/alerts, backup confidentiality, RTO/RPO, and shared
    sccache/target ownership/cleanup policy.

Until every item holds, **do not claim 100%.**

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
[`evals/persistent-agent-scenarios.v1.json`](../evals/persistent-agent-scenarios.v1.json).
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
- Complete quota observability remains a **separate** question: the live
  report must state whether Grok Build gateway quota was observed, and must
  **not** claim GrokPtah account-balance sync unless a cited main object
  implements it. Compatible gateway requests still consume provider quota.
  A merged local host ledger is still not a Grok Build balance.

**Must not claim:** “Grok Build certified” from hermetic replay or from
routing-only unit tests.

## Stage 3 — Least-privilege remote authority

**Depends on:** stage 2 (live routing/tools proven on finite Runs). **Must
complete before any production-shaped 72-hour autonomous soak.**

**Exists today:** Operator-equivalent named bearers
(ADR-002 §5, [`HEADLESS_SERVICE.md`](HEADLESS_SERVICE.md)). **Every
configured remote bearer can directly approve and promote within service
scope** (`ptah_approve_run`, `ptah_promote_run`). Computer MCP **reads** are
on main; **mutations** remain [#271](https://github.com/chriscase/GrokPtah/issues/271)
**open**.

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

**Must not claim:** “scoped tokens” while every `--client` bearer still
receives `CONTROL_TOOLS` in full. **Must not start** a production-shaped
72-hour soak on operator-equivalent bearers.

## Stage 4 — Desktop / hosted shared parity

**Depends on:** stage 3 (capability advertisement is unsafe while every
bearer is operator-equivalent).

**Exists today:** Shared runtime/protocol; Experimental parity; no declared
host capability document; hosted-service.yml **Pending — not shipped**.

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
`agent_private` / `team` descriptors, 80-fact / 800-character / 6_000-inject
bounds ([`MEMORY_SCOPES.md`](MEMORY_SCOPES.md), `memory.rs`,
`tests/memory_scopes.rs`). Team scope is denied unless host policy approves
an ID. There is **no** accelerated logical-years retention proof, no
revision/supersession/expiry/conflict protocol beyond exact-text
deduplication, no compaction/retrieval-quality eval, and no Manager frozen
memory attribution contract.

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
executor; routines (manual/schedule). Grokbot is not a binary. Unattended
Computer Use is Explicitly unsupported. Certification-lab smoke checks that
managed execution is **disabled by default**. No 72-hour soak report exists.

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
- [#305](https://github.com/chriscase/GrokPtah/issues/305) is closed with
  independent-worker proof, or explicitly descoped in the matrix.
- This soak is **operational**. It does not replace stage 5 memory evidence.

**Must not claim:** always-on Grokbot, unattended Computer Use, or soak
from desktop-focused sessions only. **Must not claim** years-long agents
from this soak.

## Stage 7 — Agent-owned Computer Use surface

**Depends on:** stages 1–2 for provider honesty; does not require stage 6
for a local-only slice, but **release** of the surface as 100% does.

**Exists today:** Experimental foreground semantic CU + cockpit projection
([`computerActivity.ts`](../desktop/src/lib/computerActivity.ts)).
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

**Exists today:** Foreground activation required (`GPTTargetIsFocused`).
[#287](https://github.com/chriscase/GrokPtah/issues/287) **open**.

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
qualify**.

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
`docs/ux-design/`. [#273](https://github.com/chriscase/GrokPtah/issues/273)
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
- [#308](https://github.com/chriscase/GrokPtah/issues/308) selected
  direction is implemented far enough that Agents vs Lanes vs finite Run
  language matches ADR-002, **or** remaining gaps are Explicitly
  unsupported in the matrix.

**Must not claim:** packaged UX certified from `tauri:dev` or terminal-owned
TCC grants.

## Stage 11 — Operations and release drills

**Depends on:** stages 2–6 and 10 (there must be something real to operate,
including least-privilege soak).

**Exists today:** Documented backup/restore and `/ready` behavior
([`HEADLESS_SERVICE.md`](HEADLESS_SERVICE.md)); deterministic service
conformance; Computer Use Stop / Take over tests in-process; compiler-cache
hygiene in [`BUILD_PERFORMANCE.md`](BUILD_PERFORMANCE.md). No dated
production-like drill report.

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
- **Shared sccache / target ownership and cleanup:** each worktree keeps a
  private Cargo target; `sccache` is optional, namespaced, and never shares
  writable targets across concurrent worktrees; cleanup deletes only the
  documented cache/target paths ([`BUILD_PERFORMANCE.md`](BUILD_PERFORMANCE.md)).
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
6 72-hour operational soak (not a memory proof)
        │
        ├──────────────► 7 agent-owned Computer Use surface
        │                         │
        │              ┌──────────┴──────────┐
        │              ▼                     ▼
        │     8 background-safe      9 isolated visual (mandatory)
        │              │                     │
        └──────────────┴──────────┬──────────┘
                                  ▼
                 10 packaged UX and accessibility certification
                                  │
                                  ▼
                 11 operations and release drills
                                  │
                                  ▼
                         trustworthy 100% claim
```

Stages 7–9 may be implemented locally after stage 2, but they **do not
count toward 100%** until stages 3–6 and 10–11 are also done. Isolated
visual (stage 9) has no Explicitly unsupported waiver.

## Unverified (explicit)

The following remain **unverified** as of 2026-08-22. They are not
shipped facts:

- Any live Grok Build campaign result (`certification_ready`, catalog IDs
  above).
- Grok Build gateway quota observability and any account-balance API.
- Packaged-identity Computer Use hardware matrix ([#274](https://github.com/chriscase/GrokPtah/issues/274)).
- Isolated visual Computer Use ([#288](https://github.com/chriscase/GrokPtah/issues/288)).
- Always-on 72-hour operational soak.
- Long-horizon / logical-years memory evidence.
- Least-privilege tokens in production-shaped configs.
- Versioned desktop-loopback vs hosted black-box parity fixture.
- Native Coding Readiness Center / local quota ledger / hosted-service CI
  until those objects exist on `origin/main` after certified P1 repair.
- Whether [#305](https://github.com/chriscase/GrokPtah/issues/305) will close
  as complete or be descoped.

See [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) for per-row evidence.
