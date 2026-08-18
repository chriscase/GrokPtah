# Phase 2 contract integration notes

This note reconciles the independent Grok Build contract review with the
Phase 1 evidence, the Agent/Lane runtime model, issue #308, issue #273, and the
Phase 2 prototype rubric. It is an integration decision record, not a claim
that the recommended lifecycle and hosted behavior are implemented today.

## Evidence precedence

When a prototype claim conflicts with another artifact, use this order:

1. Current executable behavior and repository tests.
2. Phase 1 observations from the real application.
3. Documented product contracts and issue requirements.
4. Prototype recommendations.

The prototype may demonstrate a future product contract, but it must label the
boundary and must not turn an unobserved flow into apparent evidence.

## Accepted repository facts

- A Lane is currently a product projection over a Session; its compatibility
  identity remains `lane_id = session_id`.
- One Agent can list several Lanes, but checkpoint resume is still constrained
  by the Agent's primary session and workspace. The product must not promise
  cross-Lane resume until that runtime restriction is removed.
- Agent operational health exists today. The separate `active`, `paused`, and
  `retired` lifecycle is proposed product behavior, not an implemented enum.
- Archive is durable and reversible at the data level, but opening an archived
  Session currently restores it as a side effect and archived work is not yet
  blocked from every mutation path.
- The current remote “Run on” operation submits into a different service-owned
  Session/Lane. It does not silently move the focused Lane to another Runtime.
- Hosted service persistence, multi-client ledger access, and reconnect rules
  have protocol evidence. Phase 1 did not observe a hosted end-to-end workflow,
  second-device handoff, or synchronized transcript experience.
- Provider credentials, source trees, terminals, clipboard contents, desktop
  layout, and Computer Use authority are not synchronized across devices.
- Computer Use is local, Lane-scoped operator authority. Its durable evidence
  must not imply that capture or input authority survives restart.
- Follow-up prompts, host admission, tool permission, isolated promotion, and
  Computer Use authorization are different mechanisms and need different
  user-facing names.

## Prototype corrections required before scoring

### Runtime wording

- Replace any local Runtime promise such as “Nothing is uploaded” with wording
  that distinguishes local GrokPtah persistence from requests sent to the
  configured model provider.
- A “change Runtime” action must either say that it continues the objective in
  another Lane or be marked as a future contract. It cannot imply that files,
  terminals, credentials, or a live Run move.
- A hosted fixture must say that it illustrates the documented contract and was
  not observed end to end in Phase 1.

### Agent lifecycle and ownership

- A generic Agent-level Runtime field is misleading because Runtime belongs to
  a Lane. Use a per-Lane label or an aggregate such as “2 local, 1 hosted.”
- Pause, Retire, and any Unretire action must be marked as proposed lifecycle
  behavior until the separate lifecycle field and mutation gates exist.
- Retire must preserve Lane and Run history. Whether Unretire is permitted is a
  contract decision, not current behavior; the prototype must not present it as
  an already-supported recovery action.
- Agent reassignment and retirement consequences must not imply that existing
  Lanes are automatically rewritten, archived, or moved.

### Archive, resume, and recovery

- Inspect Archived and Restore are separate actions. The desired design must
  not copy the current auto-restore side effect.
- Reconnect, Resume from checkpoint, Retry interrupted Run, and Start new Run
  are separate actions with separate preconditions.
- Cross-Lane Agent resume must be shown as blocked or future until the primary
  session/workspace restriction is removed.

### Computer Use

- The Phase 1 unavailable state is valid evidence, but it is not enough to
  evaluate issue #273 integration.
- At least one contract-labelled state must show exact target, grant scope and
  expiry, model/provider, origin, action and time budgets, current action,
  observation freshness, and always-reachable Pause, Stop, Take over, and
  non-cancelling Steer controls.
- Approval must be visibly bound to Lane, Run, action, target, and fresh
  evidence. Restart, target change, or stale observation invalidates authority.
- No successful hosted Computer Use flow may be depicted.

## Decisions adopted for Phase 2 scoring

- Agents and Lanes remain permanently reachable destinations.
- The default work experience is one focused Lane and one explicit composer.
- Multi-Lane supervision remains available as a distinct expert operation.
- Every contextual surface carries Lane, Agent or Ad hoc, Runtime, workspace,
  and current Run context.
- Empty, loading, error, and stale states are mutually coherent. An error may
  preserve a last-known non-empty list, but cannot simultaneously claim that no
  records exist.
- Archive is Lane-only and reversible. Retire is Agent-only and preserves
  attribution.
- Workloads and routines may be reserved in the information architecture but
  are not invented as durable objects in these prototypes.
- S04 and S08 are contract reviews. S14 is a contract integration review owned
  by issue #273. Neither is scored as an observed production success.

## Implementation-order consequence

The selected visual direction should be implemented in vertical slices after
the underlying ownership rules are honest:

1. Finish explicit Lane scope and normalized state grammar.
2. Correct archive inspect/restore behavior and block new archived mutations.
3. Make remote Runtime presentation honest about separate service Lanes.
4. Remove the primary-session restriction from Agent resume with fail-closed
   workspace and checkpoint validation.
5. Add separate Agent lifecycle state and mutation gates.
6. Introduce the selected navigation and visual hierarchy incrementally.

This sequence prevents a polished Agent home from promising durable multi-Lane
continuation before the runtime can enforce it.
