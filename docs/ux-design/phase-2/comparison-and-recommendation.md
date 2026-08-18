# Comparison and recommendation

This document is opinion built on top of the Phase 1 evidence. Section 1
restates the observed facts relied on; everything after it is design
judgment and is labeled as such.

Update 2026-08-18: aligned with the independent product-contract review
(`delegated/GROK-BUILD-CONTRACT-REVIEW.md`) and the integration record
(`integration/CONTRACT-INTEGRATION.md`). The independent S01–S14 evaluation,
hard gates, and weighted scores are in
`integration/DIRECTION-EVALUATION.md`. See §6 for contract constraints.

## 1. Observed facts relied on (from Phase 1 / Fable review — not new claims)

- The current default is a dense, session-first cockpit (103 sessions, 8
  Live sessions, multi-zone docking) whose ownership can visibly break
  across zones (Tools showing empty status beside a completed lane).
- Persistent Agents surfaced as a Tools diagnostic and, during the audit,
  showed a raw store-lock error *and* a "none yet" empty state at once.
- Archive exists and is reversible; Delete permanently sits in the same
  session menu; Retire (Agent) did not exist as a distinct surface.
- Computer Use was observed only in a not-granted/locked state. No hosted
  session, reconnect, or second-device handoff was observed.
- The audit did **not** measure how often real users supervise multiple
  lanes concurrently. That distribution is unknown.

## 2. Comparison (judgment)

All three directions share the same model, grammar, focused-Lane surface,
and drawers; they differ in the default orientation surface and how
multi-Lane supervision is reached.

| Criterion | D1 Focused Lane Workbench | D2 Agent Operations Home | D3 Adaptive Expert Workspace |
|---|---|---|---|
| Answers "what should I do next" fastest | **Best** — one banner, one action | Good — attention rail, then Lane | Good, but split across zones |
| Teaches the Agent/Lane model | Adequate (header + library) | **Best** — structural | Adequate (rail sections) |
| Fixes wrong-lane actions (F-03) | Best by default (one Lane) | Same as D1 in Lane view | **Best under load** — pinned Inspector makes the failure unrepresentable while multi-viewing |
| Default cognitive load | **Lowest** | Low–medium (roster) | Highest of the three |
| Quick ad-hoc task ("just run this here") | **Fastest** | Slowest (one hop) | Fast (rail) |
| Fleet/hosted operations at a glance | Weakest | **Strongest** | Strong |
| Existing power-user continuity | Medium | Medium | **Highest** |
| Migration cost to first ship | **Lowest** | Highest (needs Agent↔Lane decoupling first) | Medium, but tempting to inherit docking debt |
| Risk of recreating the audited overload | Low | Low | Real, if it ships as default |
| New-user onboarding story | **Simplest** | Strong for the durable-agent promise | Weakest |

Independent weighted result: **D1 88.75**, **D2 86.25**, **D3 77.50**. All
three pass every design hard gate; the scores select implementation order,
not three separate component systems.

## 3. Recommendation (judgment)

**Compose, don't pick a silo: ship Direction 1's focused Lane workspace as
the default work surface, adopt Direction 2's Agents area as the identity
spine, and deliver Direction 3's pinned-Inspector workspace as the opt-in
supervision mode.** The three directions were deliberately built on one
shared model and component set so this composition is a sequencing
decision, not a redesign.

If a single direction must be named the primary bet for the first
implementation slices, it is **Direction 1**, because:

1. It resolves the two P0/P1 clusters that carry the most user harm today —
   contradictory failure states (F-01, fixed by the shared grammar it ships
   with) and default overload/ownership ambiguity (F-03/F-04, fixed
   structurally by one-Lane-default) — without waiting for the
   Agent↔Lane record decoupling that D2's roster honesty requires.
2. Its migration path is the shortest distance from the current SessionPane
   and can land behind the already-planned Lane projection
   (`lane_id = session_id`).
3. Nothing in it forecloses D2 or D3: the Agents screens it links to *are*
   D2's components; its Expert Grid is D3's workspace minus the pinned
   Inspector.

Suggested sequence (aligned with the runtime model's migration strategy):

1. **Slice 1 — grammar + scope.** Ship the shared state components and the
   Lane-scoped panel contract inside the existing UI (no navigation change
   yet). This alone retires F-01 and the worst of F-03.
2. **Slice 2 — D1 shell.** Focused Lane default, rail, Lanes list/archive/
   search, lifecycle-grouped actions. Sessions sidebar behind a
   compatibility flag.
3. **Slice 3 — D2 spine.** Agent roster/detail once many-Lane resume semantics
   are honest; Retire/Pause with consequence copy; current D04 ad-hoc Build
   assignment with no routine Agent-to-Agent reassignment.
4. **Slice 4 — D3 mode.** Supervision workspace with pinned Inspector,
   replacing Live rail and dock vocabulary.
5. **Continuous:** runtime target/connection surfacing (already partially
   projected by the service work) rides in every slice's Lane header.

## 4. What would change this recommendation

- If instrumentation or the next research pass shows most sessions involve
  **concurrent supervision of 2+ lanes**, D3 should be promoted to default
  (with one-zone first-run), and D1 becomes its Focus mode — the shared
  components make this a default-flag change, not a rebuild.
- If the hosted service becomes the primary deployment before the desktop
  redesign lands, D2's roster should lead, because connection/identity
  triage becomes the daily entry question.

## 5. Unresolved design decisions (need product answers before build)

1. Canonical left-nav object for Chat-kind work (Lane with a badge vs a
   separate area) — the audit's Builds/Chats split is otherwise dissolved.
2. Zone cap and first-run zone count for the supervision mode (2 vs 3).
3. Whether Inspector pinning may ever follow user action automatically
   (opt-in convenience vs hard rule).
4. Home behavior for single-Agent and zero-Agent users in a D2-led world.
5. Where "Delete permanently" ultimately lives (a retention/settings
   surface is proposed; it must not return to row menus). Per the contract
   review, its confirmation must state that durable Agent/Run history may
   remain in the orchestration store.
6. Recovery ownership for the locked orchestration/computer-use store —
   the Phase 1 question 4 remains open and gates the final copy of the
   load-failed state ("close the other GrokPtah window" vs an automatic
   repair path).

## 6. Contract-review update (2026-08-18)

The independent contract review settles several items this document
previously left open, and adds constraints the recommendation must respect:

- **Settled dispositions:** Agents and Lanes both permanently reachable
  (D02); any future Agent assignment must be explicit and non-rewriting,
  with no routine Agent-to-Agent transfer, while attribution and checkpoint
  semantics remain a D04 contract dependency; Retire blocked while queued/running
  Runs or live isolated approvals exist, never auto-archiving Lanes (D05);
  bulk archive is Lane-only (D06); runtime change is never a silent retarget
  — continuing elsewhere is a different Lane (D11); the Resume vocabulary
  (Reconnect / Resume from checkpoint / Retry interrupted Run / Start new
  Run / Start on another Runtime) is authoritative (D15); per-zone Lane
  ownership in any grid (D18).
- **Constraints on the recommendation:** the implementation sequence in §3
  holds, with the contract's sharper gate — explicit Lane scope, state
  grammar, and the archive inspect-without-restore correction land **before**
  visual IA slices, and resume must be unbound from the Agent's primary
  session before any Agent-home surface implies many-Lane continuation
  (contract migration step 6 / D20).
- **Proposed vs implemented:** Agent lifecycle (Pause/Retire/Unretire) is a
  proposed contract; the prototype now marks it "Proposed" everywhere, and
  production must not ship the chrome before the lifecycle field and
  mutation gates exist.
