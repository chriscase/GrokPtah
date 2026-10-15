# Road to 100%

This is the dependency-ordered path from `origin/main` to a **trustworthy**
100% claim. It does not describe 100% as already met.

Status of what exists today is
[`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md). Do not treat draft
[PR #352](https://github.com/chriscase/GrokPtah/pull/352) or
[#343](https://github.com/chriscase/GrokPtah/pull/343)–[#351](https://github.com/chriscase/GrokPtah/pull/351)
as shipped.

## Distinctions that survive every stage

**GrokPtah already routes Grok Build session/gateway traffic** via
`~/.grok/auth.json` / OIDC (`auth_store.rs`, cli-chat-proxy headers). That
Supported routing is not complete quota observability and is not exact live
certification. GrokPtah does not synchronize a Grok Build account balance.
`LiveCredentialAttestation.certification_ready` is a stricter campaign gate
and is not recorded as passed.

**Current semantic Computer Use deliberately avoids raw global mouse injection** (`pointer_fallback: false`;
`unsupported_pointer_fallback_never_reaches_backend`). **Foreground activation is not equivalent to non-disruptive isolated Computer Use.** The
current slice activates the selected target and rechecks the frontmost
application (`ActivateTarget`, `GPTTargetIsFocused`). Background-safe
semantic ([#287](https://github.com/chriscase/GrokPtah/issues/287)) and an
isolated visual backend ([#288](https://github.com/chriscase/GrokPtah/issues/288))
are later stages.

**Grokbot** is epic/ADR language for always-available hosted agents
([#301](https://github.com/chriscase/GrokPtah/issues/301),
[`ADR-002-runtime-boundaries.md`](ADR-002-runtime-boundaries.md)). There is
no shipped binary named Grokbot.

## What “100%” means (measurable exit)

A 100% claim is allowed only when **all ten stages below have met their
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
   [#287](https://github.com/chriscase/GrokPtah/issues/287)) remains without
   a recorded close proof that matches its acceptance criteria.
4. A named live Grok Build campaign report is committed or attached to the
   closing issue, covering the catalog IDs in
   [`PERSISTENT_AGENT_CERTIFICATION.md`](PERSISTENT_AGENT_CERTIFICATION.md)
   that this roadmap requires, with `certification_ready == true` for that
   campaign’s credential binding.
5. An always-on hosted soak report exists (duration, restart count, zero
   implicit resumes, bounded resource growth).
6. Remote credentials are no longer operator-equivalent for approval,
   promotion, or Computer Use mutation.
7. Desktop and `grokptah-service` advertise a declared capability document
   and fail closed on missing host capabilities; hosted-service CI runs on
   `origin/main`.
8. Computer Use has an agent-owned interaction surface, a background-safe
   semantic tier **or** an explicit documented unsupported disposition with
   tests, and an isolated visual backend **or** an explicit documented
   unsupported disposition with tests. Raw global input remains Explicitly
   unsupported unless isolation is actually proven.
9. Packaged UX and accessibility certification for the Computer cockpit and
   the selected product UX direction ([#273](https://github.com/chriscase/GrokPtah/issues/273),
   [#308](https://github.com/chriscase/GrokPtah/issues/308)) is recorded.
10. Operations and release drills have a dated runbook execution (backup,
    restore, restart, cursor expiry, credential rotation, Computer Use Stop /
    Take over on a packaged identity).

Until every item holds, **do not claim 100%.**

## Stage 1 — Merge-blocker repair

**Depends on:** nothing (current `origin/main`).

**Exists today:** Desktop CI
([`.github/workflows/desktop.yml`](../.github/workflows/desktop.yml)).
Native Coding RC and hosted-service CI live only on draft PR #352.
[#277](https://github.com/chriscase/GrokPtah/issues/277) (nanoid lockfile
audit) is **open**. Epic [#301](https://github.com/chriscase/GrokPtah/issues/301)
checkboxes are stale versus closed children.

**Exit (all required):**

- This matrix/roadmap is on `origin/main` and allowlisted docs no longer
  contradict it.
- [#277](https://github.com/chriscase/GrokPtah/issues/277) is closed with
  `npm audit --json` reporting zero findings on the desktop lockfile, or an
  explicit documented residual with owner sign-off.
- Native Coding RC ([PR #352](https://github.com/chriscase/GrokPtah/pull/352))
  is either merged **after** its capabilities are no longer labeled pending
  in the matrix, or remains draft with every capability still **Pending —
  not shipped**.
- Source drafts [#343](https://github.com/chriscase/GrokPtah/pull/343)–[#351](https://github.com/chriscase/GrokPtah/pull/351)
  are not described as shipped.
- Open issues [#305](https://github.com/chriscase/GrokPtah/issues/305) and
  [#308](https://github.com/chriscase/GrokPtah/issues/308) are not called
  complete.

**Must not claim:** Native Coding Readiness Center, local quota ledger, or
hosted-service.yml as shipped while they exist only on PR #352.

## Stage 2 — Live Grok Build certification

**Depends on:** stage 1 (no contradictory docs; RC either pending or merged
honestly).

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
  implements it.

**Must not claim:** “Grok Build certified” from hermetic replay or from
routing-only unit tests.

## Stage 3 — Always-on Grokbot certification and soak

**Depends on:** stage 2 (live routing/tools proven on finite Runs).

**Exists today:** Experimental manager supervisor + hosted home; native
executor; routines (manual/schedule). Grokbot is not a binary. Unattended
Computer Use is Explicitly unsupported. Certification-lab smoke checks that
managed execution is **disabled by default**.

**Exit (all required):**

- A hosted `grokptah-service` instance remains the sole writer of one
  `GROKPTAH_HOME` for a declared soak window of at least **72 hours**, with
  at least **three** process restarts.
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

**Must not claim:** always-on Grokbot, unattended Computer Use, or soak
from desktop-focused sessions only.

## Stage 4 — Least-privilege remote authority

**Depends on:** stage 3 (always-on home is real enough to need scoped
tokens).

**Exists today:** Operator-equivalent named bearers
(ADR-002 §5, [`HEADLESS_SERVICE.md`](HEADLESS_SERVICE.md)). Computer MCP
**reads** are on main; **mutations** remain [#271](https://github.com/chriscase/GrokPtah/issues/271)
**open**.

**Exit (all required):**

- Each credential maps to a transport-neutral `AuthorityContext` (principal,
  credential id, tier, workspace/Agent scope, permitted operations).
- At least one non-operator tier **cannot** call `ptah_approve_run`,
  `ptah_promote_run`, Computer Use mutation, or managed-execution enablement.
- Tests prove: wrong tier → typed forbidden; caller-supplied Agent ID is
  not authentication; Computer Use grants still require the local privileged
  operator on a capable host.
- [#271](https://github.com/chriscase/GrokPtah/issues/271) mutations stay
  disabled until this authority model and the threat review both pass.

**Must not claim:** “scoped tokens” while every `--client` bearer still
receives `CONTROL_TOOLS` in full.

## Stage 5 — Desktop / hosted shared parity

**Depends on:** stage 4 (capability advertisement is unsafe while every
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
- Conformance suite on a hosted-shaped config covers session create/list,
  submit/cancel, restart, cursor expiry, and Computer **read** authorization.
- Desktop remote client still cannot inherit Computer Use or keychain from
  the service.

**Must not claim:** “parity complete” from sharing a crate without declared
capabilities and hosted CI.

## Stage 6 — Agent-owned Computer Use surface

**Depends on:** stages 1–2 for provider honesty; does not require stage 5
for a local-only slice, but **release** of the surface as 100% does.

**Exists today:** Experimental foreground semantic CU + cockpit projection
([`computerActivity.ts`](../desktop/src/lib/computerActivity.ts)).
[#286](https://github.com/chriscase/GrokPtah/issues/286) **open**.

**Exit:** every [#286](https://github.com/chriscase/GrokPtah/issues/286)
acceptance criterion, including: user pointer unchanged; agent cursor only
inside the authorized surface; persistent Stop / Take over; no raw pointer
fallback introduced by the surface layer.

**Must not claim:** Codex-like Computer Use from the current cockpit
preview.

## Stage 7 — Background-safe semantic execution

**Depends on:** stage 6 (activity/attention events without OS-pointer
takeover).

**Exists today:** Foreground activation required (`GPTTargetIsFocused`).
[#287](https://github.com/chriscase/GrokPtah/issues/287) **open**.

**Exit:** every [#287](https://github.com/chriscase/GrokPtah/issues/287)
acceptance criterion, including: a supported background action leaves
foreground app, active window, and physical pointer unchanged; unsupported
targets require explicit foreground authorization; no silent raw-input
fallback.

**Must not claim:** background-safe CU because Accessibility `invoke`
exists. Foreground activation is not this stage.

## Stage 8 — Isolated visual backend

**Depends on:** stage 6 (agent-owned pointer contract). Stage 7 is not a
substitute.

**Exists today:** `ComputerUseTier::VisualFallbackAct` is not granted by
the first probe. [#288](https://github.com/chriscase/GrokPtah/issues/288)
**open**. Hidden windows, Spaces, and global `CGEvent` injection **do not
qualify**.

**Exit:** every [#288](https://github.com/chriscase/GrokPtah/issues/288)
acceptance criterion **or** the matrix row is marked Explicitly unsupported
with a recorded product decision. User physical pointer, foreground app,
and clipboard unchanged; agent pointer has no OS-global side effect.

**Must not claim:** isolated visual CU from screenshots of the live desktop.

## Stage 9 — Packaged UX and accessibility certification

**Depends on:** stages 6–8 for Computer Use UX; stage 5 for hosted/desktop
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

## Stage 10 — Operations and release drills

**Depends on:** stages 2–5 and 9 (there must be something real to operate).

**Exists today:** Documented backup/restore and `/ready` behavior
([`HEADLESS_SERVICE.md`](HEADLESS_SERVICE.md)); deterministic service
conformance; Computer Use Stop / Take over tests in-process. No dated
production-like drill report.

**Exit (all required):**

- A written runbook is executed at least once on a disposable packaged
  desktop **and** a disposable hosted service: stop, copy `GROKPTAH_HOME`,
  restore to one writer, verify `/ready`, inspect interrupted Runs, explicit
  resume only.
- Credential rotation: API-key and OIDC-principal change invalidate
  measured qualifications as documented; ordinary access-token refresh does
  not.
- Computer Use drill: Pause, Stop, Take over, permission revocation on the
  packaged identity.
- Cursor-expiry and MCP reconnect drill matches `service_conformance.rs`
  guarantees.
- Release artifact (notarization is still a documented non-goal unless this
  stage explicitly adds it) is identified by version, not by a commit SHA
  treated as an immortal product fact.

**Must not claim:** operations certified from unit tests alone.

## Dependency graph (summary)

```text
1 merge-blocker repair
        │
        ▼
2 live Grok Build certification
        │
        ▼
3 always-on Grokbot certification and soak
        │
        ▼
4 least-privilege remote authority
        │
        ▼
5 desktop/hosted shared parity
        │
        ├──────────────► 6 agent-owned Computer Use surface
        │                         │
        │              ┌──────────┴──────────┐
        │              ▼                     ▼
        │     7 background-safe      8 isolated visual backend
        │              │                     │
        └──────────────┴──────────┬──────────┘
                                  ▼
                 9 packaged UX and accessibility certification
                                  │
                                  ▼
                 10 operations and release drills
                                  │
                                  ▼
                         trustworthy 100% claim
```

Stages 6–8 may be implemented locally after stage 2, but they **do not
count toward 100%** until stages 4–5 and 9–10 are also done.

## Unverified (explicit)

The following remain **unverified** as of this document. They are not
shipped facts:

- Any live Grok Build campaign result (`certification_ready`, catalog IDs
  above).
- Grok Build gateway quota observability and any account-balance API.
- Packaged-identity Computer Use hardware matrix ([#274](https://github.com/chriscase/GrokPtah/issues/274)).
- Always-on soak.
- Least-privilege tokens in production-shaped configs.
- Native Coding Readiness Center / local quota ledger / hosted-service CI
  until those objects exist on `origin/main`.
- Whether [#305](https://github.com/chriscase/GrokPtah/issues/305) will close
  as complete or be descoped.

See [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) for per-row evidence.
