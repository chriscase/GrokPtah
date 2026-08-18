# Phase 2 requirements traceability

This table is the completion ledger for GrokPtah issue #308 Phase 2. A row is
complete only when the named repository artifact or reviewed prototype provides
direct evidence; plans and model claims are not proof.

## Phase 2 goal ledger

| Requirement | Required evidence | Current evidence | Status |
| --- | --- | --- | --- |
| Three audit-grounded prototype directions | Three distinguishable, reviewable directions tied to Phase 1 findings | Fable 5 work in progress | Pending |
| Focused Lane default explored | Prototype shows one dominant Lane, explicit composer target, and contextual expert tools | Awaiting prototype | Pending |
| Agent-centric operation explored | Prototype shows durable Agent roster/detail and one-Agent-to-many-Lanes relationship | Awaiting prototype | Pending |
| Expert multi-Lane operation explored | Prototype preserves supervision power while showing scope on every zone | Awaiting prototype | Pending |
| Representative local workflows evaluated | S01–S03, S05–S07, and S09–S14 have prototype locators and findings | `SCENARIO-RUBRIC.md` defines the test | Pending review |
| Representative hosted workflows evaluated | S04 and S08 are evaluated against documented contracts without claiming unobserved success | `SCENARIO-RUBRIC.md` defines the evidence boundary | Pending review |
| Shared lifecycle/state model | Agent, Lane, Run, Runtime, workspace, archive, interruption, checkpoint, and retirement states are mapped | `../delegated/GROK-BUILD-CONTRACT-REVIEW.md` §§1–3 plus `CONTRACT-INTEGRATION.md` | Contract complete; prototype conformance pending |
| Local/hosted ownership boundaries | Matrix distinguishes desktop-owned, local-service-owned, hosted-service-owned, and client-only state | `../delegated/GROK-BUILD-CONTRACT-REVIEW.md` §4 plus `CONTRACT-INTEGRATION.md` | Complete for documented contracts; hosted observation still pending |
| Comparable evaluation | All directions receive hard-gate and weighted score evidence | `SCENARIO-RUBRIC.md` and `REVIEW-WORKSHEET.md` | Framework complete; scoring pending |
| Recommended direction selected | Decision record names a direction or hybrid and explains rejected alternatives | Awaiting completed prototypes and scoring | Pending |
| Accessibility and narrow behavior specified | Prototype and design package cover keyboard, focus, semantics, contrast, zoom, reduced motion, and narrow widths | Requirements defined in rubric | Pending prototype review |
| Migration is incremental | Recommendation identifies independently reviewable vertical slices over current contracts | Awaiting delegated analyses | Pending |
| Package is repository-owned | All final documents, prototype source, deterministic captures, and decision records are committed | Integration branch exists with framework only | Partial |
| Production UI remains unchanged | Final branch diff contains design/documentation artifacts only | Current Phase 2 branch adds only `docs/ux-design/phase-2/` | Currently satisfied; recheck before publication |
| Reviewable work is published | Clean branch, validated artifact links/captures, and PR against the correct base | Not yet published | Pending |

## Issue #308 acceptance-criteria crosswalk

| Issue criterion | Phase 2 proof target | Status |
| --- | --- | --- |
| Current-state UX audit and screenshot baseline are committed | Parent PR #309 contains Phase 1 audit and 20 screenshots | Published but parent CI failing; not merged |
| Core jobs and usability failures are ranked | Phase 1 prioritized findings plus S01–S13 | Evidence available |
| IA distinguishes home, workspace, Agent, Lane/session, workload, routine, and Run | Final README, glossary, and selected-direction IA | Pending |
| Distinct Agents and Lanes areas with predictable lifecycle language | All viable prototypes pass lifecycle hard gate | Pending |
| One Agent can have many Lanes; archive preserves durable work | Agent detail, archive/restore prototype, state matrix | Pending |
| Local and hosted concepts are understandable without identical layouts | Runtime/ownership matrix and S04/S08 review | Pending |
| At least two prototype directions reviewed against the same scenarios | Three directions scored with the same framework | Pending |
| Selected direction has rationale, accessibility requirements, inventory, and migration plan | Final decision record and package index | Pending |
| Existing functionality and expert workflows are mapped before removal | S05–S07 and S14 plus tool/component inventory | Pending |
| Implementation children follow design review | No production implementation or child slice should be opened before decision | Satisfied so far |
| Each future slice has visual, interaction, accessibility, and regression criteria | Final migration roadmap template | Pending |
| Computer Use requirements are integrated rather than duplicated | Prototype presents Computer Use as Lane-scoped contextual work and retains #273 ownership | Pending review |

## Publication gate

Before Phase 2 is marked complete:

1. Replace every `Pending` row with a direct artifact link and review result.
2. Verify the static prototype from a clean checkout without external network
   dependencies.
3. Capture desktop and narrow renderings for each direction.
4. Confirm each hard gate against a concrete screen or transition.
5. Record weighted scores and the recommendation rationale.
6. Compare the final branch with its base and prove that no production source or
   build configuration changed.
7. Publish the branch and ensure its required checks are green or explicitly
   documented as non-code artifact limitations.
