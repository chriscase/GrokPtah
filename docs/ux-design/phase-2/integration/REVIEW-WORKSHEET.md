# Phase 2 prototype review worksheet

Use one copy of the direction section for each independent reviewer. Record
prototype locators rather than relying on memory or general impressions.

## Review metadata

| Field | Value |
| --- | --- |
| Direction | |
| Prototype revision | |
| Reviewer | |
| Date | |
| Window sizes reviewed | |
| Input methods reviewed | |

## Scenario coverage

| Scenario | Complete | Prototype locator | Missing state or ambiguity |
| --- | :---: | --- | --- |
| S01 First launch and setup | | | |
| S02 Ad-hoc local Lane | | | |
| S03 One Agent, several Lanes | | | |
| S04 Hosted Agent home | | | |
| S05 Supervise active Lanes | | | |
| S06 Queue and steering | | | |
| S07 Diff, tests, and approval | | | |
| S08 Service disconnect recovery | | | |
| S09 Interrupted Run resume | | | |
| S10 Archive and restore Lane | | | |
| S11 Pause and retire Agent | | | |
| S12 Search and history | | | |
| S13 Narrow and keyboard-first | | | |

## Hard gates

| Gate | Pass | Evidence locator | Required correction |
| --- | :---: | --- | --- |
| Explicit Lane ownership | | | |
| Archive/Retire distinction | | | |
| Non-contradictory state grammar | | | |
| Honest Runtime and synchronization model | | | |
| Disconnection and interruption recovery | | | |
| Historical context preserved | | | |
| Progressive disclosure | | | |
| Accessible structure | | | |

## Weighted scoring

Scores range from 0 to 4. Weighted points equal `score / 4 * weight`.

| Criterion | Weight | Score | Weighted points | Evidence |
| --- | ---: | ---: | ---: | --- |
| Immediate comprehensibility | 15 | | | |
| Agent/Lane lifecycle clarity | 15 | | | |
| Lane ownership and safety | 15 | | | |
| Visual hierarchy and focus | 10 | | | |
| Local/hosted clarity | 10 | | | |
| Recovery and state grammar | 10 | | | |
| Expert workflow preservation | 10 | | | |
| Accessibility and narrow layout | 10 | | | |
| Migration feasibility | 5 | | | |
| **Total** | **100** | | | |

## Direction summary

### Strongest decisions

1.
2.
3.

### Highest-risk decisions

1.
2.
3.

### Repository contradictions

Record any prototype claim that conflicts with current contracts or observed
behavior. Do not score an invented capability as though it exists.

### Valuable ideas to preserve if this direction is rejected

1.
2.
3.

### Verdict

- [ ] Adopt
- [ ] Hybridize
- [ ] Reject

Rationale:

## Cross-direction decision table

Complete this only after all directions have independent reviews.

| Decision area | Focused Lane Workbench | Agent Operations Home | Adaptive Expert Workspace | Selected approach |
| --- | --- | --- | --- | --- |
| Default landing experience | | | | |
| Primary navigation | | | | |
| Agent roster and detail | | | | |
| Lane list and archive | | | | |
| Focused Lane workspace | | | | |
| Multi-Lane supervision | | | | |
| Runtime target presentation | | | | |
| State and recovery grammar | | | | |
| Narrow-window behavior | | | | |
| Expert tools | | | | |
| Migration sequence | | | | |
