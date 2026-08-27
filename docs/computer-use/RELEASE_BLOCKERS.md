# Computer Use — Release-Blocker Checklist

A checkbox may be ticked only by a **dated, reproducible artifact pinned to an exact head SHA**.
Source existing, a green PR, or a passing demo is never sufficient — the same bar
`docs/ROADMAP_TO_100.md` already sets for the project as a whole.

Status at the gate (`8ad3be07`): **0 of 57 closed.**

---

## A. Correctness of the safety kernel

- [ ] **A1** No action dispatched without a live lease, grant, exact target, fresh frame, and allowed class — proven by test, not by inspection.
- [ ] **A2** No action dispatched on a stale observation. *(Kernel mechanism exists at `service.rs:428`; not benchmark-proven.)*
- [ ] **A3** `operator_takeover` is absorbing under every race, including takeover during dispatch. *(Mechanism exists; needs adversarial coverage.)*
- [ ] **A4** Restart during any state yields `interrupted` + cleared authority + `uncertain` receipts. *(Mechanism exists; needs the full state matrix.)*
- [ ] **A5** Idempotency: duplicate `request_id` replays, conflicting payload rejects, expiry is bounded.
- [ ] **A6** Budget exhaustion is terminal and non-negotiable; no model path can widen a budget.
- [ ] **A7** Profile monotonicity: every check firing in economy also fires in balanced and high-assurance (CU-P1-14).
- [ ] **A8** All 41 blockers re-verified on the exact release candidate, not on the branch that introduced each.

## B. Identity and verification

- [ ] **B1** `ElementKey` stable across resize, DPI change, display change, app restart, list reorder, and localization.
- [ ] **B2** `LOST` re-anchor rate ≤ 5 % on the corpus.
- [ ] **B3** `AMBIGUOUS` never resolves to a silent guess — always abstain or escalate.
- [ ] **B4** Every dispatched intent carries an `Expectation`; no code path can dispatch without one.
- [ ] **B5** `CONTRADICTED` never auto-retries a mutation, in any profile, under any budget.
- [ ] **B6** Verifier fidelity ≥ action fidelity, enforced in policy and covered by test.
- [ ] **B7** `UNVERIFIABLE` ≤ 25 % of steps on the corpus; two consecutive ends the run.

## C. Model boundary

- [ ] **C1** No fixed adversarial string in any production prompt (**CU-P0-09**; `computer_agent.rs:283` and `provider_qualification.rs:427` today).
- [ ] **C2** Model-visible frame bounded per profile; **the ceiling-based 10,000-element / 8 MiB path at `computer_agent.rs:216` is removed**.
- [ ] **C3** Grammar, JSON Schema, and regex constraints generated from one source; drift is impossible by construction.
- [ ] **C4** At most one grammar repair per step; a second failure abstains.
- [ ] **C5** Every local route passes all 10 qualification probes, pinned to a fingerprint that **includes quantization**.
- [ ] **C6** Qualification is cleared by any change to model, endpoint, dialect, quantization, or grammar version.
- [ ] **C7** Model-authored `rationale` is never re-fed into a later prompt.
- [ ] **C8** Irreversible intents require `high` confidence in every profile, and mandatory escalation at economy tier.

## D. Privacy and injection

- [ ] **D1** Secure/system-restricted surfaces never reach a model — **0 occurrences** on the corpus.
- [ ] **D2** Secret detection runs before any model sees OCR or AX text; leak corpus is clean.
- [ ] **D3** Screenshot bytes never leave the host below high-assurance; enforced in policy, not convention.
- [ ] **D4** Per-run egress ledger accounts for **100 %** of model calls with byte counts and route.
- [ ] **D5** All 12 adversarial injection families pass with zero out-of-scope actions.
- [ ] **D6** Held-out adversarial set (never used in development) passes at the same bar.

## E. Authority, arbitration, isolation

- [ ] **E1** Leases enforced, not DTO-only (`grokptah-agent-sdk/src/computer.rs:20` has **zero consumers** today).
- [ ] **E2** `Foreground` domain strictly serialized machine-wide; two agents never interleave.
- [ ] **E3** Human preemption succeeds at every point in the lease lifecycle.
- [ ] **E4** No deadlock under ordered acquisition + TTL; proven by a contention soak.
- [ ] **E5** **CU-P0-01 closed** — one isolated-visual substrate chosen, the other formally abandoned.
- [ ] **E6** Guest provides a genuinely separate input surface. *(Hidden windows, separate Spaces, and global `CGEvent` injection do not qualify — existing threat-model constraint, preserved.)*
- [ ] **E7** Guest lifecycle leak-free under failure, restart, cancel, and takeover.
- [ ] **E8** Guest cleanup verified by an independent process check, not by the guest's own report.

## F. Platform and packaging

- [ ] **F1** Three-action macOS fixture proof through the **packaged** identity with real Screen Recording and Accessibility grants. *(Terminal-owned grants do not prove packaged identity — existing blocker.)*
- [ ] **F2** Hardware matrix complete: focus theft, app restart, window reuse, resize, DPI/display change, occlusion, permission revocation.
- [ ] **F3** Signed and notarized artifacts; reproducible build from a clean worktree with a recorded SHA.
- [ ] **F4** Signed helper identity verified at launch; an unsigned helper fails closed.
- [ ] **F5** Target attestation defeats a look-alike app adopting the reviewed title and icon.

## G. Product and accessibility

- [ ] **G1** Every mutation requires a visible one-use local approval. *(Exists at `desktop/src-tauri/src/computer_use.rs:605`; must survive the profile work.)*
- [ ] **G2** Cockpit surfaces verification verdict, confidence, abstention reason, and remaining budget.
- [ ] **G3** Full keyboard operation of every approval and takeover control, no mouse required.
- [ ] **G4** Screen-reader announcement for every state transition, verified with a real screen reader in the **packaged** app.
- [ ] **G5** Reduced motion, forced colors, and large text verified in the packaged app.
- [ ] **G6** An unrecognized control disposition fails closed in the UI. *(Exists at `computerActivity.ts:131`.)*

## H. Measurement and CI

- [ ] **H1** Step records emitted for **100 %** of steps, including deterministic ones.
- [ ] **H2** Benchmark corpus ≥ 20 fixtures per active class, ≥ 3 seeds, oracle-checked, hermetic.
- [ ] **H3** All `PROFILES_AND_THRESHOLDS.md` §2 thresholds met **per class**, with 95 % CIs.
- [ ] **H4** Every §2.4 absolute at exactly zero.
- [ ] **H5** Root Cargo workspace built and tested in CI (**CU-P0-03** — today `grokptah-agent-sdk` and every `xai-computer-hub-*` crate are compiled by **no** workflow).
- [ ] **H6** Bridge builds on Linux in CI (**CU-P0-04**).
- [ ] **H7** Fixture validator wired into CI (`docs/computer-use/fixtures/validate.py`).
- [ ] **H8** Benchmark runs on every PR touching `computer_use/**`, with regressions blocking.
- [ ] **H9** Independent strongest-model review passed against exact base/head SHAs, all findings resolved or explicitly accepted.

---

## Blocking dependencies among the blockers

```
CU-P0-01 (E5) ──► E6, E7, E8, F4
CU-P0-05 ──────► B1, B2, B3 ──► B4..B7 ──► H3
CU-P0-13 ──────► H1 ──► H3, H4, D4
CU-P0-03/04 ───► H5, H6
CU-P0-09 ──────► C1 ──► D5, D6
CU-P0-10 ──────► C2 ──► H3
```

**E5 (`CU-P0-01`) is the cheapest blocker to close and gates four others.** It is a decision, and it
should be made this week.

## Explicitly out of scope for 1.0

Recorded so they are not mistaken for oversights:

- Windows and Linux adapters (P2; #275/#276).
- Unattended or continuously autonomous operation.
- Cross-application targets inside a single run.
- MCP-exposed Computer mutations. *(Existing constraint: keep #271 mutations disabled until the shared event/approval contract and its threat review are complete.)*
- Raw shell, clipboard, AppleScript, and arbitrary coordinate endpoints.
