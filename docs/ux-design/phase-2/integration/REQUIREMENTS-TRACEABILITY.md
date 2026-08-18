# Phase 2 requirements traceability

This table is the completion ledger for GrokPtah issue #308 Phase 2. A row is
complete only when the named repository artifact or reviewed prototype provides
direct evidence; plans and model claims are not proof.

## Phase 2 goal ledger

| Requirement | Required evidence | Current evidence | Status |
| --- | --- | --- | --- |
| Three audit-grounded prototype directions | Three distinguishable, reviewable directions tied to Phase 1 findings | `../direction-1-focused-lane-workbench.md`, `../direction-2-agent-operations-home.md`, `../direction-3-adaptive-expert-workspace.md`, and `../prototype/` | Complete |
| Focused Lane default explored | Prototype shows one dominant Lane, explicit composer target, and contextual expert tools | `#/d1/lane/lane-1`, D1 captures, and `DIRECTION-EVALUATION.md` | Complete |
| Agent-centric operation explored | Prototype shows durable Agent roster/detail and one-Agent-to-many-Lanes relationship | `#/d2/agents`, `#/d2/agent/agent-2`, D2 captures, and evaluation | Complete |
| Expert multi-Lane operation explored | Prototype preserves supervision power while showing scope on every zone | `#/d3/workspace`, desktop and 760×2200 captures, and evaluation | Complete |
| Representative local workflows evaluated | S01–S03, S05–S07, and S09–S14 have prototype locators and findings | `DIRECTION-EVALUATION.md` scenario table | Complete with S01/S14 gaps explicitly deferred |
| Representative hosted workflows evaluated | S04 and S08 are evaluated against documented contracts without claiming unobserved success | `DIRECTION-EVALUATION.md`, `CONTRACT-INTEGRATION.md`, and inline fixture labels | Complete as contract review; end-to-end observation deferred |
| Shared lifecycle/state model | Agent, Lane, Run, Runtime, workspace, archive, interruption, checkpoint, and retirement states are mapped | `../components-and-states.md`, delegated review §§1–3, and `CONTRACT-INTEGRATION.md` | Complete at design/contract level |
| Local/hosted ownership boundaries | Matrix distinguishes desktop-owned, local-service-owned, hosted-service-owned, and client-only state | `../delegated/GROK-BUILD-CONTRACT-REVIEW.md` §4 plus `CONTRACT-INTEGRATION.md` | Complete for documented contracts; hosted observation still pending |
| Comparable evaluation | All directions receive hard-gate and weighted score evidence | `DIRECTION-EVALUATION.md`: D1 88.75, D2 86.25, D3 77.50 | Complete |
| Recommended direction selected | Decision record names a direction or hybrid and explains rejected alternatives | `DIRECTION-EVALUATION.md` selects D1 work surface + D2 spine + opt-in D3 mode | Complete |
| Accessibility and narrow behavior specified | Prototype and design package cover keyboard, focus, semantics, contrast, zoom, reduced motion, and narrow widths | `../components-and-states.md`, evaluation hard gate, and three direction-specific narrow captures | Complete at design level; production AT testing required |
| Migration is incremental | Recommendation identifies independently reviewable vertical slices over current contracts | `IMPLEMENTATION-ROADMAP.md` | Complete |
| Package is repository-owned | All final documents, prototype source, deterministic captures, and decision records are committed | Entire package is under `docs/ux-design/phase-2/` on the integration branch | Complete after final commit |
| Production UI remains unchanged | Final branch diff contains design/documentation artifacts only | Current Phase 2 branch adds only `docs/ux-design/phase-2/` | Currently satisfied; recheck before publication |
| Reviewable work is published | Clean branch, validated artifact links/captures, and PR against the correct base | Draft PR [#311](https://github.com/chriscase/GrokPtah/pull/311) targets `codex/lane-context-foundation-20260817` | Complete; inherited parent CI state documented in PR |

## Issue #308 acceptance-criteria crosswalk

| Issue criterion | Phase 2 proof target | Status |
| --- | --- | --- |
| Current-state UX audit and screenshot baseline are committed | Parent PR #309 contains Phase 1 audit and 20 screenshots | Published but parent CI failing; not merged |
| Core jobs and usability failures are ranked | Phase 1 prioritized findings plus S01–S13 | Evidence available |
| IA distinguishes home, workspace, Agent, Lane/session, workload, routine, and Run | README glossary/IA reserves settings and future workload/routine concepts without inventing durable objects | Complete for Phase 2 scope |
| Distinct Agents and Lanes areas with predictable lifecycle language | Every direction passes lifecycle hard gate; proposed lifecycle is visibly labelled | Complete |
| One Agent can have many Lanes; archive preserves durable work | Agent detail, archive/restore routes, state inventory, and contract review | Complete at design level |
| Local and hosted concepts are understandable without identical layouts | Runtime/ownership matrix, inline contract labels, and S04/S08 review | Complete as contract design |
| At least two prototype directions reviewed against the same scenarios | Three directions scored with the same S01–S14 framework | Complete |
| Selected direction has rationale, accessibility requirements, inventory, and migration plan | `DIRECTION-EVALUATION.md`, `../components-and-states.md`, and `IMPLEMENTATION-ROADMAP.md` | Complete |
| Existing functionality and expert workflows are mapped before removal | S05–S07/S14, shared drawer inventory, D3 Inspector, and roadmap regression criteria | Complete |
| Implementation children follow design review | No production implementation or child slice should be opened before decision | Satisfied so far |
| Each future slice has visual, interaction, accessibility, and regression criteria | `IMPLEMENTATION-ROADMAP.md` | Complete |
| Computer Use requirements are integrated rather than duplicated | Lane-bound observed/contract tabs retain #273 ownership and refuse a mismatched Lane | Complete at design-contract level |

## Publication gate

Before Phase 2 is marked complete:

1. Replace every Phase 2 design `Pending` row with a direct artifact link and review result.
2. Verify the static prototype from a clean checkout without external network
   dependencies.
3. Capture desktop and narrow renderings for each direction.
4. Confirm each hard gate against a concrete screen or transition.
5. Record weighted scores and the recommendation rationale.
6. Compare the final branch with its base and prove that no production source or
   build configuration changed.
7. Publish the branch and ensure its required checks are green or explicitly
   documented as non-code artifact limitations.
