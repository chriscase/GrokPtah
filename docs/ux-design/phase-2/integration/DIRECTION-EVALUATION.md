# Phase 2 direction evaluation and decision record

Review date: 2026-08-18  
Scope: GrokPtah issue #308 Phase 2 static design package  
Evidence boundary: design evaluation only; no production UI, hosted end-to-end,
or successful Computer Use run is claimed.

## Outcome

All three directions pass the eight design hard gates and exceed the 70-point
viability threshold. The recommendation is a staged hybrid:

1. **Direction 1 — Focused Lane Workbench** is the default work surface.
2. **Direction 2 — Agent Operations Home** supplies the permanent Agents
   destination and durable-identity spine.
3. **Direction 3 — Adaptive Expert Workspace** becomes an opt-in supervision
   mode with a pinned Inspector, not the default first-run experience.

If one direction must be named for the first implementation slice, select
Direction 1. It removes the audited density and wrong-Lane risks with the
fewest runtime prerequisites while preserving routes into the other two.

## Score summary

Scores use the 0–4 rubric in `SCENARIO-RUBRIC.md`; weighted points equal
`score / 4 × weight`.

| Criterion | Weight | D1 score | D1 points | D2 score | D2 points | D3 score | D3 points |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Immediate comprehensibility | 15 | 4 | 15.00 | 3 | 11.25 | 2 | 7.50 |
| Agent/Lane lifecycle clarity | 15 | 3 | 11.25 | 4 | 15.00 | 3 | 11.25 |
| Lane ownership and safety | 15 | 4 | 15.00 | 4 | 15.00 | 4 | 15.00 |
| Visual hierarchy and focus | 10 | 4 | 10.00 | 3 | 7.50 | 3 | 7.50 |
| Local/hosted clarity | 10 | 3 | 7.50 | 4 | 10.00 | 3 | 7.50 |
| Recovery and state grammar | 10 | 4 | 10.00 | 4 | 10.00 | 3 | 7.50 |
| Expert workflow preservation | 10 | 3 | 7.50 | 3 | 7.50 | 4 | 10.00 |
| Accessibility and narrow layout | 10 | 3 | 7.50 | 3 | 7.50 | 3 | 7.50 |
| Migration feasibility | 5 | 4 | 5.00 | 2 | 2.50 | 3 | 3.75 |
| **Total** | **100** |  | **88.75** |  | **86.25** |  | **77.50** |

The accessibility score is capped at 3 for every direction because the static
prototype specifies and demonstrates semantics, focus behavior, reduced
motion, and narrow layouts, but cannot prove production screen-reader or
streaming-announcement behavior.

## Scenario coverage

Legend: **Complete** means the prototype provides a coherent route or
transition; **Partial** names a material missing state; **Contract** means the
screen deliberately evaluates documented behavior rather than observed
end-to-end production evidence. Shared focused-Lane, Agent, Lane-list, and
Runtime screens are available to all directions.

| Scenario | D1 Focused Lane | D2 Agent Home | D3 Expert Workspace | Evidence and finding |
| --- | --- | --- | --- | --- |
| S01 First launch/setup | Partial | Partial | Partial | `#/d2/agents?demo=empty` distinguishes empty from `?demo=error` and offers ad-hoc work, but provider/authentication setup is not prototyped. |
| S02 Ad-hoc local Lane | Complete | Complete, one extra hop | Complete through Focus | `#/d1/lane/lane-3`; explicit Lane, workspace, Runtime, and composer target. D04 assignment is Build-only, non-rewriting, and exposes the primary-resume limitation. |
| S03 One Agent, many Lanes | Complete, secondary | **Complete, strongest** | Complete in rail/shared detail | `#/d2/agents` and `#/d2/agent/agent-2`; lifecycle and health remain separate, Runtime is per Lane. |
| S04 Hosted Agent home | Contract | **Contract, strongest** | Contract | `#/d2/agent/agent-2`, `#/d3/runtime`; service ownership and non-synchronized state are explicit. Authentication, tenancy, and second-device handoff remain unobserved. |
| S05 Supervise active Lanes | Complete via opt-in Grid | Complete via attention rail, Grid remains separate | **Complete, strongest** | `#/d1/grid` and `#/d3/workspace`; D3 provides self-labelled zones and a pinned Inspector but is denser. |
| S06 Queue and steering | Complete | Complete in focused Lane | **Complete in Inspector/focus** | `?drawer=queue`; steering is separate from queued prompts and each mutation is Lane-scoped. Conflict/receipt transitions remain descriptive fixtures. |
| S07 Diff, tests, approval | Complete | Complete | **Complete in pinned Inspector/focus** | `#/d1/lane/lane-2?drawer=approvals` and D3 Changes & tests; Run and fingerprint scope stay visible. |
| S08 Service disconnect | Contract | **Contract, strongest** | Contract | `#/d2/lane/lane-10` and `#/d1/lane/lane-2?conn=stale`; reconnect does not claim the service Run stopped. |
| S09 Interrupted checkpoint | Complete | Complete | Complete through Focus | `#/d1/lane/lane-4`; Resume, Retry, and Inspect are distinct and remain in the Lane. |
| S10 Archive/restore Lane | Complete | **Complete, strongest** | Complete through shared library | `#/d2/lane/lane-6` and `#/d2/lanes/archived`; inspection does not auto-restore and history is named. |
| S11 Pause/retire Agent | Complete via Agents | **Complete, strongest** | Complete via Agents | `#/d2/agent/agent-4` and Retire dialog; every lifecycle action is labelled proposed, blocked by live work, and distinct from Archive. |
| S12 Search/history | Complete | **Complete, strongest** | Complete via shared library | `#/d2/lanes/all`; result rows retain Agent, workspace, Runtime, Run indicators, and archive state. |
| S13 Narrow/keyboard-first | Complete at design level | Complete at design level | Complete at design level | 760px captures show D1 focus, D2 single-column roster, and D3 stacked zones plus Inspector. Semantic controls and focus return are implemented; production AT testing is deferred. |
| S14 Computer Use control | Contract, partial negative-state set | Contract through focused Lane | Contract through Focus | `?drawer=computer&cu=contract` binds target, Lane, Run, grant, budget, evidence, and controls. The audited unavailable state is separate. Additional revoked/lost/expired variants remain issue #273 work. |

## Hard gates

| Gate | D1 | D2 | D3 | Concrete evidence |
| --- | :---: | :---: | :---: | --- |
| Explicit Lane ownership | Pass | Pass | Pass | Context header and composer target on focused Lanes; every drawer starts with a scope line; D3 zones self-label and Inspector pin never follows focus. The Computer Use contract fixture refuses to render under a different Lane. |
| Archive/Retire distinction | Pass | Pass | Pass | Lane archive/restore routes and Agent-only proposed retirement dialog use different locations, language, and consequences. |
| Non-contradictory state grammar | Pass | Pass | Pass | `?demo=empty` and `?demo=error` replace one another; blocked, stale, interrupted, queued, and archived fixtures use one primary state. |
| Honest Runtime/synchronization model | Pass | Pass | Pass | Runtime screen distinguishes local persistence from provider traffic and labels hosted/VM frames as unobserved contract fixtures. Continuing elsewhere creates/selects another Lane. |
| Disconnection/interruption recovery | Pass | Pass | Pass | VM Reconnect, stale event recovery, verified-checkpoint Resume, and Retry are distinct actions. |
| Historical context preserved | Pass | Pass | Pass | Archived Lane banner/list retain transcript, Runs, checkpoints, approvals, evidence, Agent, and workspace context. |
| Progressive disclosure | Pass | Pass | Pass with risk | D1 drawers and D2 focused work keep one primary task; D3 caps the prototype at two zones and offers Exit to focused Lane, but remains unsuitable as the default. |
| Accessible structure | Pass at design level | Pass at design level | Pass at design level | Landmarks, semantic controls, focus-visible, heading focus without viewport shift, reduced motion, labelled states, and reviewed narrow captures. Production screen-reader verification remains required. |

## Direction verdicts

### D1 — adopt as the work surface

Best decisions: one dominant Lane; one explicit composer; consequence-first
state banner; scoped drawers; expert Grid remains one deliberate action away.

Primary risks: durable Agent identity is secondary, the five-Lane rail needs an
ordering rule, and frequent supervisors take an extra step into Grid.

### D2 — adopt as the identity spine

Best decisions: Agent/Lane structure teaches itself; lifecycle and health are
separate; Runtime is summarized from Lanes; attention and history roll up well
for hosted operation.

Primary risks: quick ad-hoc work pays an extra navigation hop; a one-Agent user
sees ceremony; honest multi-Lane checkpoint continuation depends on removing
the primary-session/workspace restriction. Agent lifecycle must remain visibly
non-production until its separate field and mutation gates land.

### D3 — adopt only as expert supervision mode

Best decisions: per-zone ownership and pinned Inspector make wrong-Lane tools
structurally difficult; multi-Lane diff/approval supervision is strongest;
narrow layout stacks instead of compressing columns.

Primary risks: two zones plus Inspector still impose the highest initial load;
two composers increase targeting risk; Inspector tabs and focused-Lane drawers
could drift into duplicated interaction patterns.

## Evidence gaps and deferrals

- S01 still needs a provider/authentication and optional-hosted first-launch
  prototype before implementation copy is final.
- S04 and S08 remain documented-contract reviews until a hosted deployment,
  reconnect, and second-device handoff are exercised.
- S14 remains owned by issue #273. The successful operator surface is a
  contract illustration; additional revoked, target-lost, sensitive-surface,
  limit, expiry, and restart states still require implementation evidence.
- D04 permits initial ad-hoc Build assignment, but routine reassignment and
  cross-Lane checkpoint continuation remain blocked. Agent lifecycle
  implementation, D10 synchronization enforcement, and cross-Lane resume are
  contract/runtime dependencies.
- Real usage frequency for simultaneous supervision is unknown. Instrument
  concurrent Lane use before considering D3 as the default.
- Static checks cannot prove VoiceOver/NVDA behavior, long-list virtualization,
  streaming live regions, or production color contrast after implementation.

## Decision

**Hybridize, with D1 first.** D2 outperforms D1 on durable identity and hosted
triage; D3 outperforms both on simultaneous supervision. Those strengths are
preserved as permanent destinations and a separate mode. Neither advantage
justifies reintroducing a roster or cockpit as the default surface for a user
who is trying to advance one objective.

The implementation acceptance sequence is defined in
`IMPLEMENTATION-ROADMAP.md`.
