# Milestones — 2-Week Dogfood and Qualification Roadmap

Baseline: `codex/external-worker-hardening-v1` @ `8ad3be07`.
Dates are relative working days from lane start. Every milestone has a **falsifiable exit artifact**;
"the code exists" is never an exit condition — that is the failure mode `ROADMAP_TO_100.md` already
warns about, and this plan holds itself to the same bar.

---

## M1 — 2-Week Dogfood (days 1–10)

**Goal:** one operator, one macOS machine, one application class (`native-semantic`), running real
multi-step tasks with measurement — not a demo.

**Deliberately excluded from M1:** local small models, OCR, vision, DOM, isolation, arbitration,
Windows, Linux, profiles. Adding any of them makes the two weeks unachievable and the result
unmeasurable.

### Scope

| Day | Work | Lane |
|---|---|---|
| 1 | CU-P0-01 substrate decision recorded | L-TRIAGE |
| 1–2 | CU-P0-03/04 CI jobs green | L-CI |
| 1–2 | CU-P0-09 canary removed from production prompts | L-PROMPT |
| 1–6 | CU-P0-05 `ElementKey` + anchors + 30 tests | L-IDENT |
| 1–8 | CU-P0-14 corpus (20 fixtures) + offline runner | L-BENCH |
| 3–8 | CU-P0-10 `CompactFrame` + narrowing | L-FRAME |
| 4–9 | CU-P0-13 step records + cost ledger | L-TELEM |
| 6–9 | CU-P0-06 anchored diff (behind the existing default) | L-IDENT |
| 7–10 | CU-P0-07 `Expectation` + verification loop | L-KERNEL |
| 9–10 | CU-P0-08 deterministic rules (subset: activate, settle, focus-before-type) | L-IDENT |
| 10 | Dogfood run + baseline report | all |

CU-P0-02 (the isolation-stack rebase) runs in parallel on L-KERNEL but is **not** an M1 exit
requirement — it is large and its failure must not take the dogfood with it.

### M1 exit artifacts

1. **Baseline report** at an exact head SHA covering ≥ 20 `native-semantic` fixtures × 3 seeds, with:
   task success, step verification rate, **silent-failure rate**, wrong-action rate, candidate recall
   @K=12, deterministic-step fraction, tokens/step, p50/p95 latency, cost/task.
2. **Operator log**: ≥ 30 real tasks attempted by a human on a real machine, with every abstention and
   every wrong action recorded verbatim.
3. **A written list of what broke** — the most valuable M1 output. If M1 produces no surprises, the
   fixtures are too easy and must be hardened before M2.
4. Identity suite green: resize, DPI change, app restart, list reorder, localization, duplicate labels,
   element removal.
5. Zero occurrences on every `PROFILES_AND_THRESHOLDS.md` §2.4 absolute.

### M1 go/no-go

| Signal | Go | No-go response |
|---|---|---|
| Candidate recall @K=12 | ≥ 0.95 | narrowing is broken → fix before M2; do **not** proceed to small models |
| Deterministic-step fraction | ≥ 25 % | efficiency thesis is weak → re-scope v2 as a safety-only story |
| Silent-failure rate | ≤ 3 % | verification is not working → M2 is verification repair, not new features |
| Re-anchor `LOST` rate | ≤ 5 % | identity is not stable → CU-P0-05 reopens |

**M1 is explicitly allowed to fail.** Its purpose is to test the architecture's central claim —
that narrowing plus verification carries the load — at the smallest scope where that claim is testable.

---

## M2 — Small-model viability (weeks 3–6)

**Goal:** prove or disprove that a 1–8B local model can run economy mode on `native-semantic`.

Work: CU-P0-11 grammar SSOT · CU-P0-12 abstain/confidence · CU-P1-09 local runtime + 10-probe
qualification · CU-P1-06 router · CU-P1-07 escalation/budgets · CU-P1-08 stationary ladder ·
CU-P1-14 profiles.

Exit artifacts:

1. ≥ 3 local models qualified through all 10 probes, each pinned to a route fingerprint **including
   quantization**.
2. Economy-mode report against every §2 threshold, per class.
3. **Confidence calibration curve** — measured accuracy per band. If `high` is not materially more
   accurate than `med`, the bands are noise and the confidence floors are meaningless.
4. Head-to-head: same corpus, same fixtures, economy vs balanced vs a pixel-driven baseline.

**M2 go/no-go:** economy silent-failure ≤ 1 % and abstention precision ≥ 0.70. If not met after
calibration, economy mode ships as **propose-only** (never authorizes a mutation alone) or does not
ship. It does not ship with a lowered bar.

---

## M3 — Adapter breadth (weeks 6–10)

CU-P1-01 fidelity types · CU-P1-02 DOM adapter · CU-P1-03 OCR · CU-P1-04 vision fallback ·
CU-P1-18 secret detection · CU-P1-19 egress ledger.

Exit artifacts: per-class thresholds met for `web-dom`; OCR proven locate-only by test; vision
fallback exercised only under high-assurance with a complete egress ledger; secret-detection corpus
with zero leaks.

---

## M4 — Authority and arbitration (weeks 8–12, overlaps M3)

CU-P0-02 (if not already landed) · CU-P1-10 leases · CU-P1-11 envelopes · CU-P1-12 arbiter ·
CU-P1-13 replay.

Exit artifacts: two agents contending for `Foreground` on one machine, serialized with zero
interleaved actions; human preemption at every point in the lease lifecycle; deterministic replay
byte-identical at temperature 0.

---

## M5 — Isolation and packaging (weeks 10–16)

CU-P1-21 signed helper + packaged identity · CU-P1-22 guest lifecycle · leak-freedom gates ·
guest soak.

Exit artifacts: the existing release blockers from `COMPUTER_USE_THREAT_MODEL.md`, closed —
packaged-identity fixture proof, hardware matrix for focus/geometry/display/permission-revocation,
genuine separate input surface for the guest, and clean cleanup under failure, restart, and takeover.

---

## M6 — Qualification (weeks 16–20)

Full protocol from `PROFILES_AND_THRESHOLDS.md` §4 across all three profiles and all six application
classes, plus:

- Held-out adversarial set the tuning never saw.
- Independent strongest-model review against exact base/head SHAs (`INDEPENDENT_REVIEW_PROTOCOL.md`).
- Accessibility conformance in the **packaged** app, not a dev build.
- Long-horizon soak (CU-P2-10).

---

## Risk register

| Risk | Likelihood | Impact | Response |
|---|---|---|---|
| `ElementKey` unstable on real macOS apps | medium | **critical** | M1 measures `LOST` rate explicitly; falls back to observation-scoped identity with a documented reliability ceiling |
| Candidate recall unreachable at K=12 | medium | **critical** | raise K per class; classes that still miss become ineligible for economy |
| Local models cannot hold the grammar | medium | high | constrained decoding is mandatory, not prompt-based; models that fail probe 1 are simply not qualified |
| The two substrates both keep growing | **high** | high | CU-P0-01 is day 1 and blocks two lanes |
| Benchmark measures the wrong thing | medium | **critical** | silent-failure rate is primary; §5 falsifiers are written before results exist |
| Verification is `UNVERIFIABLE` too often | medium | high | measured in M1; > 25 % means verification is theater and M2 becomes adapter repair |
| Scope creep into Windows/Linux before macOS is qualified | **high** | high | P2 by construction; no lane assigned before M6 |
