# Parallel Lane Plan

Goal: run 5–6 lanes concurrently without two lanes writing the same file in the same window.
Contention in this codebase is concentrated in four files —
`computer_use/{types,policy,service,store}.rs` — so the plan is built around **when** those files are
open, not only around who owns what.

**Branch prefix convention (existing repo practice):** `codex/`, `cursor/`, `claude/`, `grok/`, `fable/`.
One lane, one prefix, one active branch. Every lane rebases on the gate SHA, never on `main`
(`main` @ `67e29bd` has no common recent ancestry with the gate; merge-base is `127ffaf`).

---

## Wave 0 — unblock (days 1–3, strictly serial where noted)

| Lane | Issue | Files | Serial? |
|---|---|---|---|
| **L-TRIAGE** | CU-P0-01 substrate decision | `docs/computer-use/SUBSTRATE_DECISION.md` | **blocks L-KERNEL, L-GUEST** |
| **L-CI** | CU-P0-03, CU-P0-04 | `.github/workflows/*` (new files only) | parallel, no conflict |
| **L-PROMPT** | CU-P0-09 remove canary | `computer_agent.rs`, `provider_qualification.rs`, `host.rs` | parallel |

Wave 0 exists because two of its three items are cheap and unblock everything else. CU-P0-01 is a
decision, not an implementation; it must not be scheduled as if it were engineering work.

---

## Wave 1 — foundations (weeks 1–3)

```
L-KERNEL   CU-P0-02  land isolation/attestation/lease/domain stack (ordered rebase)
           ── owns types.rs, policy.rs, service.rs, store.rs EXCLUSIVELY in this wave
L-IDENT    CU-P0-05  ElementKey + anchors        ── new file identity.rs
           CU-P0-06  anchored diff + staleness   ── identity.rs + policy.rs (AFTER L-KERNEL lands)
L-FRAME    CU-P0-10  CompactFrame + narrowing    ── new file frame.rs
L-GRAMMAR  CU-P0-11  grammar SSOT                ── new file grammar.rs
L-BENCH    CU-P0-14  corpus + runner             ── evals/computer-use/** only
```

Conflict management in Wave 1:

- **L-KERNEL holds the four contended files.** No other lane may touch `types.rs`, `policy.rs`,
  `service.rs`, or `store.rs` until CU-P0-02 lands. L-IDENT and L-FRAME work entirely in new modules and
  land their `mod.rs` wiring as a single small follow-up each.
- L-IDENT's `policy.rs` change (CU-P0-06) is explicitly scheduled **after** L-KERNEL completes.
- L-GRAMMAR and L-PROMPT both touch `computer_agent.rs`. Serialize: **L-PROMPT (09) → L-GRAMMAR (11)**.
- L-BENCH has zero overlap with any code lane and can start on day 1.

---

## Wave 2 — decision layer and verification (weeks 3–5)

```
L-KERNEL   CU-P0-07  Expectation + verification  ── verify.rs (new) + service.rs
L-IDENT    CU-P0-08  deterministic rules         ── rules.rs (new)
L-DECIDE   CU-P0-12  abstain + confidence        ── computer_agent.rs (after L-GRAMMAR)
L-TELEM    CU-P0-13  step records + cost ledger  ── telemetry.rs (new) + store.rs + projection.rs
L-BENCH    CU-P0-14  baseline report on the gate SHA
```

`store.rs` is touched by both L-KERNEL (Wave 1) and L-TELEM (Wave 2). Because CU-P0-02 lands at the end
of Wave 1, L-TELEM is safe to start Wave 2. Order within Wave 2: **07 → 13** if both need `service.rs`.

---

## Wave 3 — adapters and product (weeks 5–9)

```
L-ADAPT    CU-P1-01 fidelity types → CU-P1-02 DOM adapter → CU-P1-03 OCR
L-DECIDE   CU-P1-06 router → CU-P1-07 escalation/budgets → CU-P1-08 stationary ladder
L-LOCAL    CU-P1-09 local model runtime + 10-probe qualification
L-AUTH     CU-P1-10 real leases → CU-P1-11 envelope unification
L-PRODUCT  CU-P1-14 profiles → CU-P1-15 cockpit → CU-P1-16 a11y → CU-P1-17 rename
L-GUEST    CU-P1-22 guest lifecycle (only after CU-P0-01)
```

Wave 3 contention:

- L-AUTH and L-ADAPT both touch `policy.rs`. Split by function: L-AUTH owns grant/lease predicates,
  L-ADAPT owns the fidelity table. If that seam proves too fine, serialize L-AUTH first — leases are on
  more downstream critical paths.
- L-PRODUCT owns the entire `desktop/` tree. No other lane writes `desktop/`.
- L-LOCAL owns `gateway_config.rs` and `provider_qualification.rs` exclusively.

---

## Lane ownership table

| Lane | Exclusive files | Never touches |
|---|---|---|
| **L-KERNEL** | `computer_use/{types,policy,service,store}.rs`, `verify.rs` | `desktop/**`, `evals/**`, `.github/**` |
| **L-IDENT** | `computer_use/{identity,rules}.rs` | `service.rs` except one `mod.rs` wiring commit |
| **L-FRAME** | `computer_use/frame.rs` | `types.rs` |
| **L-GRAMMAR** | `computer_use/grammar.rs` | `service.rs`, `store.rs` |
| **L-DECIDE** | `computer_agent.rs`, `computer_use/router.rs` | `computer_use/{service,store}.rs` |
| **L-TELEM** | `computer_use/telemetry.rs` | `policy.rs` |
| **L-ADAPT** | `computer_use/{dom_adapter,ocr,platform,macos_observation}.rs` | `service.rs` |
| **L-AUTH** | `crates/common/grokptah-agent-sdk/src/computer.rs` | `desktop/**` |
| **L-PRODUCT** | `desktop/**` | `crates/**` |
| **L-LOCAL** | `gateway_config.rs`, `provider_qualification.rs` | `computer_use/**` |
| **L-BENCH** | `evals/computer-use/**`, `docs/computer-use/fixtures/**` | all source |
| **L-CI** | `.github/workflows/**` | all source |
| **L-GUEST** | `computer_use/isolated_*`, `macos_isolated_*` | `service.rs` |

---

## Integration discipline

1. **One PR per issue.** CU-P0-02 is the exception: one PR per rebased commit, each green independently.
2. **Every PR rebases on the gate SHA**, never on `main`.
3. **A lane that needs a contended file files a request rather than editing it.** The owning lane makes
   the change. Cross-lane edits to `service.rs` are the most likely way this plan fails.
4. **New modules are wired into `mod.rs` in a separate, single-line-per-lane commit** so `mod.rs` never
   causes a real conflict.
5. **No lane merges to `main`.** The gate-derived integration branch is the target; `main` is out of
   scope for this program until the qualification roadmap says otherwise.
6. **A lane blocked for > 2 days on another lane's file escalates rather than forking the file.**

---

## Recommended agent assignment

Matched to observed strengths across the existing branch history in this repository.

| Lane | Recommended | Rationale |
|---|---|---|
| **L-IDENT** (CU-P0-05/06) | **Claude Opus, xHigh** | Highest-consequence design work; needs invariant reasoning and an adversarial test suite, not throughput |
| **L-KERNEL** (CU-P0-02) | **Cursor GPT-5.6 Luna xHigh** | A 25-commit ordered rebase across five branches with duplicate content — mechanical precision over long context |
| **L-GRAMMAR / L-DECIDE** | **Claude Opus** | Grammar/schema/regex must stay provably in sync; subtle correctness |
| **L-FRAME** | **Claude Fable** | Well-specified scorer with hard testable properties; good throughput fit |
| **L-TELEM** | **Claude Fable** | Additive, low-contention, schema-driven |
| **L-ADAPT** (DOM/OCR) | **Cursor GPT-5.6 Luna xHigh** | Large integration surface, external protocol (CDP), high line count |
| **L-LOCAL** | **Claude Opus** | Provider-boundary security work; qualification must not become a rubber stamp |
| **L-PRODUCT** | **Claude Fable** | Existing desktop suite is strong; incremental TS/React with tests |
| **L-BENCH** | **Claude Opus** | Benchmark design decides what "better" means — a measurement error here invalidates everything downstream |
| **L-CI** | **Claude Fable** | Small, mechanical, additive |
| **L-GUEST** | **Cursor GPT-5.6 Luna xHigh** | 11k–25k lines of existing substrate to reconcile and harden |
| **L-TRIAGE** (CU-P0-01) | **Claude Opus, xHigh** | A judgment call with large downstream cost; needs to read both substrates properly |

---

## Failure modes this plan is designed against

| Risk | Mitigation |
|---|---|
| Two lanes rewrite `service.rs` | L-KERNEL holds it exclusively through Wave 1 |
| The substrate decision drifts and both branches keep growing | CU-P0-01 is Wave 0 and blocks two lanes |
| Identity work starts after the layers that depend on it | CU-P0-05 is day 1, strongest agent, no other work in its files |
| Benchmark arrives after the features it should have shaped | L-BENCH starts day 1 in parallel |
| CI gaps hide breakage in the SDK/hub crates | CU-P0-03/04 are Wave 0 |
| Lanes rebase on `main` and inherit an unrelated tree | rule 2, enforced in review |
