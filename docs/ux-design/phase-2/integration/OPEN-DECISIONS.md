# Phase 2 open product decisions

These decisions must be resolved or explicitly deferred before issue #308 moves
from design exploration to production implementation. They are intentionally
phrased without assuming that one prototype direction is correct.

## Decision status vocabulary

- **Decide in Phase 2:** enough evidence exists to choose a direction now.
- **Prototype and test:** the alternatives need comparative prototype evidence.
- **Contract first:** runtime or persistence behavior must be resolved before
  UI language can be final.
- **Defer with boundary:** a future hosted or device workflow is required, but
  the current design must state what it does not prove.

## Information architecture

### D01 — What is the default landing destination?

**Status:** Prototype and test.

Alternatives:

- A combined Work home that prioritizes attention and recent Lanes.
- Agents home, emphasizing persistent identities.
- Lanes home, emphasizing current objectives.
- Restore the last focused Lane when it remains valid, otherwise use Work home.

Evidence required:

- S01, S03, S05, and S12 completion quality.
- Behavior when no Agent exists but an ad-hoc Lane does.
- Behavior when all Lanes are archived but active Agents remain.

### D02 — Are Agents and Lanes both permanent top-level destinations?

**Status:** Decide in Phase 2.

The design must support direct access to both lifecycles. A combined Work home
may summarize them, but it cannot make archived Lanes or retired Agents
unreachable.

### D03 — Where does multi-Lane supervision live?

**Status:** Prototype and test.

Alternatives:

- A dedicated expert Grid destination.
- A temporary multi-Lane mode inside Work.
- An Agent operations view showing all Lanes for one identity.

The default focused experience must not inherit the density of the current
cockpit merely because expert supervision remains available.

## Agent and Lane lifecycle

### D04 — Can an ad-hoc Lane be assigned to an Agent after creation?

**Status:** Contract first.

The runtime model allows an ad-hoc Lane, but reassignment semantics need a
documented identity and history rule before the prototype presents this as a
routine action.

Questions:

- Is the assignment append-only history or only current ownership?
- May a Lane be reassigned between Agents?
- How are previous Agent contributions attributed?

### D05 — What blocks or qualifies Agent retirement?

**Status:** Contract first.

The design should preview consequences for active Runs, queued work, pending
approvals, routines, and historical Lanes. Until the contract is final, the
prototype should demonstrate the decision point without inventing automatic
cleanup.

### D06 — What can be archived in bulk?

**Status:** Decide in Phase 2.

Bulk archive should operate on Lanes selected by visible criteria such as
completion, age, project, or Agent. It must not offer a superficially similar
bulk-retire action for Agents.

### D07 — What is the primary human-readable Lane identity?

**Status:** Decide in Phase 2.

Use a concise objective/title, Agent or Ad hoc attribution, project/workspace
display name, and status. Raw session IDs and `.tmp*` paths are secondary
technical details.

## Runtime and hosted operation

### D08 — Where is Runtime target selected?

**Status:** Prototype and test.

The selected target must be visible before prompt submission. Alternatives are
a persistent Lane header control, a creation-time choice with a visible status
control, or a composer-adjacent target summary with a guarded change action.

### D09 — What constitutes an Agent home in hosted operation?

**Status:** Defer with boundary.

The conceptual design may show a hosted home reachable from multiple devices,
but authentication, tenancy, service discovery, and cross-device consistency
are not proven by the desktop audit. Phase 2 must identify these as service
requirements rather than visual facts.

### D10 — What synchronizes across devices?

**Status:** Contract first.

The design must distinguish service-owned durable state from local-only state.
It may not imply that credentials, source files, terminal processes, clipboard
contents, or Computer Use authority synchronize without an explicit contract.

### D11 — Can a Lane change Runtime target after creation?

**Status:** Contract first.

If allowed, the design needs migration, workspace compatibility, in-flight Run,
credential, and event-history rules. If not allowed, “Change target” should be
presented as creating or moving to a new Lane rather than a harmless selector.

## Work, evidence, and recovery

### D12 — What is the default content of a focused Lane?

**Status:** Prototype and test.

Candidates include transcript/progress, a task-oriented activity timeline, or a
split conversation/evidence surface. The selection must preserve a single clear
composer target and a visible current Run.

### D13 — How are attention states prioritized?

**Status:** Decide in Phase 2.

Suggested order for prototype evaluation:

1. Approval or explicit operator decision required.
2. Interrupted work with a verified recovery action.
3. Missing workspace or disconnected Runtime requiring repair.
4. Failed Run with evidence.
5. Queued work.
6. Healthy running work.
7. Completed work ready for archive.

The final order must be tested against S05 rather than accepted from this
suggestion alone.

### D14 — How much technical detail is visible by default?

**Status:** Decide in Phase 2.

Primary state copy should name impact and next action. Paths, transport errors,
cursor values, bridge versions, and operating-system codes belong in Technical
details or an exportable diagnostic record. Code, terminal output, and diff
content remain first-class when the user intentionally opens those tools.

### D15 — What is “resume” in each recovery state?

**Status:** Contract first.

The UI must distinguish:

- reconnecting a client to service-owned work;
- resuming from a verified checkpoint;
- retrying an interrupted or failed Run;
- creating a new Run in the same Lane;
- starting a replacement Lane when the workspace or Runtime is incompatible.

## Navigation and responsive behavior

### D16 — What persists when a contextual tool is opened?

**Status:** Decide in Phase 2.

Lane identity, Runtime, workspace, current Run, and the composer target must not
be obscured by opening Terminal, MCP, Computer Use, Diff/Tests, Approvals, or
Run history.

### D17 — What becomes a drawer at narrow widths?

**Status:** Prototype and test.

The prototypes should compare drawers, full-screen secondary routes, and
collapsible regions. Compressing the current desktop columns until labels wrap
or controls disappear is not an acceptable strategy.

### D18 — How does expert Grid preserve scope?

**Status:** Decide in Phase 2.

Every zone needs its own Lane ownership label. A global tab focus may assist
navigation but cannot be the implicit authority used by tools or composer
actions.

## Visual system and implementation

### D19 — How much of the current visual identity remains?

**Status:** Prototype and test.

Preserve GrokPtah identity where it communicates trust and technical focus, but
do not retain equal-weight borders, colors, and panel treatments solely for
continuity. The prototypes should demonstrate meaningful hierarchy changes.

### D20 — What is the first implementation slice after approval?

**Status:** Decide after scoring.

Candidate slices:

- New Work/Agents/Lanes navigation shell using read-only projections.
- Agent home and detail with Lane relationships.
- Focused Lane header and explicitly scoped contextual tools.
- Shared state/recovery component grammar.
- Archive/restore experience.

The selected first slice must be independently reviewable and must not require
an application-wide rewrite.

## Required Phase 2 disposition

The final recommendation must mark every decision as one of:

- decided, with rationale and prototype evidence;
- contract dependency, with named owner and blocking question;
- deferred, with an explicit product boundary;
- rejected, with the alternative that replaces it.

## Final Phase 2 dispositions

This table supersedes the exploratory status lines above. “Contract” means
the product rule is selected even when production enforcement is incomplete;
“deferred” keeps an explicit evidence boundary.

| ID | Final disposition | Evidence or owner |
| --- | --- | --- |
| D01 | **Decided:** restore the last valid focused Lane; otherwise enter the D1 work surface/Lanes attention context. Agents remains a permanent destination, not the universal home. | D1 score and S01/S03/S05 review in `DIRECTION-EVALUATION.md`; first-launch provider setup remains follow-up design. |
| D02 | **Decided:** Agents and Lanes are both permanent top-level destinations. | All viable directions and contract review §9.4. |
| D03 | **Decided:** multi-Lane supervision is a dedicated, opt-in D3 workspace with an exit to Focus. | D3 is strongest for S05 but scores 77.50 and carries the highest default load. |
| D04 | **Contract:** an ad-hoc Build Lane may be explicitly assigned to an Agent without rewriting history. No Chat attachment or routine Agent-to-Agent reassignment. Primary resume Lane/workspace does not change yet. | `attach_session_to_agent`; contract review §2.3; #297 owns attributable reassignment/cross-Lane resume. |
| D05 | **Contract:** retirement is blocked by queued/running Runs or live isolated approvals; it never archives, deletes, moves, reassigns, or rewrites Lanes. UI remains labelled proposed until lifecycle state and gates exist. | Contract review §§2.3–2.4; Agent runtime/data implementation owner. |
| D06 | **Decided:** bulk archive applies only to Lanes selected by visible criteria; there is no bulk retire analogue. | Contract review D06 and S10. |
| D07 | **Decided:** title/objective + Agent or Ad hoc + project display name + state; raw ids and paths live in Technical details. | D1 rail, Lane rows, scratch-workspace capture. |
| D08 | **Decided:** choose Runtime at Lane creation and display it in the Lane header and composer before every send. | D2 Agent detail, focused Lane header/composer, Runtime screen. |
| D09 | **Deferred with boundary:** a hosted Agent home is conceptual until authentication, tenancy, discovery, consistency, and second-device operation are observed. | S04 contract review; hosted service/product owner. |
| D10 | **Contract:** only service-owned Runs, transcripts, checkpoints, and ledger metadata synchronize as documented. Source, credentials, terminals, clipboard, desktop layout, and Computer Use authority do not. | Contract review §4 and Runtime screen. |
| D11 | **Contract:** a Lane is not silently retargeted. Continue elsewhere creates or selects a different Lane; files, terminals, credentials, and in-flight Runs do not move. | Composer dialog, disconnected recovery, Runtime screen. |
| D12 | **Decided:** focused Lane defaults to context header, state banner when needed, transcript/progress, one composer, and contextual evidence drawers. | D1 route and 88.75 score. |
| D13 | **Decided:** approval/decision → verified interruption → workspace/runtime repair → failed → queued → running → completed/archive-ready. | S05 fixtures and contract review D13. |
| D14 | **Decided:** show impact and next action first; expose paths, transport details, cursors, bridge versions, and OS errors under Technical details. | Shared StateCard/banner grammar and Phase 1 findings. |
| D15 | **Contract:** Reconnect, Resume checkpoint, Retry Run, Start new Run, and Continue in another Lane are different operations with different preconditions. | Contract review §2.6 and recovery fixtures. |
| D16 | **Decided:** Lane, Agent/Ad hoc, Runtime, workspace, Run, and composer target remain visible or explicitly named when any contextual tool opens. | Drawer scope line, context header, D3 pin. |
| D17 | **Decided:** D1/D2 contextual tools become overlay drawers at narrow widths; D3 zones and Inspector stack in reading order. | Three direction-specific narrow captures. |
| D18 | **Decided:** every expert zone owns its Lane; the Inspector changes only through its pin control; focus is never mutation authority. | D3 desktop/narrow captures and S05 hard gate. |
| D19 | **Decided:** retain GrokPtah’s dark technical identity and gold accent while reducing equal-weight panels, raw internals, and ambient chrome. | All prototype directions and comparison. |
| D20 | **Decided:** first ship explicit Lane scope + shared state grammar + archive correction inside the existing shell; then D1, Lanes library, D2, Runtime/hosted clarity, D3, and #273. | `IMPLEMENTATION-ROADMAP.md`. |
