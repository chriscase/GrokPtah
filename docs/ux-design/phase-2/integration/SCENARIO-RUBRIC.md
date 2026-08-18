# Phase 2 scenario and evaluation framework

This framework is the common test harness for the three GrokPtah Phase 2
prototype directions. It prevents visual preference from replacing product
evidence: every direction is reviewed against the same jobs, states, and
runtime constraints.

The framework evaluates design proposals only. It does not claim that a
prototype proves production behavior, hosted availability, persistence, or
accessibility implementation.

## Evidence hierarchy

When sources disagree, reviewers use this order:

1. Current repository contracts and executable behavior.
2. Observations from the real-application Phase 1 walkthrough.
3. Issue #308 product requirements.
4. Claude/Fable 5 audit findings.
5. Prototype recommendations and aesthetic preference.

The first two categories are facts. The final two are design inputs and must be
labelled as recommendations.

## Product objects that must remain distinct

| Object | Lifetime | Primary user question | Lifecycle action |
| --- | --- | --- | --- |
| Agent | Months or years | Who is responsible, and what persists with them? | Pause or retire |
| Lane | Hours to weeks | What objective and workspace are we operating in? | Archive or restore |
| Run | Minutes to hours | What execution is active, blocked, or complete? | Cancel, retry, or resume |
| Runtime target | Selected per Lane | Where will the next action execute? | Connect, reconnect, or change target |
| Workspace | Bound to a Lane | What files and branch/worktree can this work touch? | Repair or choose another workspace |

A prototype fails the review if it collapses Agent, Lane, and Run into one
indistinct status or presents Archive Lane and Retire Agent as equivalent.

## Representative operating contexts

Each scenario is reviewed in the contexts that apply:

- **Local desktop:** the desktop owns execution and local workspace access.
- **Local service/VM:** a service owns execution while remaining under the
  operator's control.
- **Hosted service:** a remote service owns execution and may be reached from
  multiple clients.
- **Narrow desktop:** the window is constrained enough that secondary panels
  cannot remain permanently visible.
- **Keyboard-first:** the critical path is navigable and understandable without
  relying on pointer-only affordances.

Hosted scenarios are contract reviews until a real hosted environment is
observed. Prototype copy must not imply source, credential, or state
synchronization beyond the documented service contract.

## Shared scenarios

### S01 — First launch and runtime setup

**Starting state:** no configured provider, no Agents, and no active Lanes.

**User objective:** understand what GrokPtah does, configure the minimum needed
to begin locally, and know that hosted operation is optional.

**Required evidence:**

- Empty state and load failure are visually and semantically distinct.
- The primary action is clear without exposing bridge versions or raw paths.
- Provider/authentication setup is not confused with Runtime target selection.
- The user can defer creating a durable Agent and start ad hoc work.

### S02 — Start an ad-hoc local Lane

**Starting state:** a valid local repository is available; no Agent assignment
is desired.

**User objective:** create a Lane, verify its workspace and branch/worktree,
send a prompt, and open the terminal or files for the same Lane.

**Required evidence:**

- The composer target is explicit before submission.
- Lane, workspace, branch/worktree, and Local desktop target are visible.
- Terminal, files, Git, MCP, and tools cannot appear owned by another Lane.
- “Ad hoc” is understandable and does not look like an error or incomplete setup.

### S03 — One durable Agent with several Lanes

**Starting state:** a long-lived maintainer Agent owns two active Lanes and
several archived Lanes.

**User objective:** inspect the Agent's current responsibility, open either
active Lane, and find historical work without confusing identity with work.

**Required evidence:**

- One-Agent-to-many-Lanes is visible in both Agent and Lane views.
- Agent health is not inferred from one Lane's last Run.
- Archived Lane history remains reachable without dominating active work.
- A new Lane can be created from the Agent context.

### S04 — Connect to a hosted Agent home

**Starting state:** a hosted Runtime target exists but is disconnected.

**User objective:** connect, understand what will execute remotely, and open a
service-owned Lane from another device.

**Required evidence:**

- Disconnected, reconnecting, connected, and error states are distinct.
- The UI names the executing Runtime and service ownership.
- The design does not imply that credentials or source files synchronize unless
  the contract guarantees it.
- Technical diagnostics are available without replacing user-oriented recovery
  guidance.

### S05 — Supervise several active Lanes

**Starting state:** three Lanes have different states: running, queued, and
awaiting approval.

**User objective:** identify which work needs attention, open the correct Lane,
and understand each next action.

**Required evidence:**

- Attention is prioritized without turning every state into an alert.
- Agent, Lane, Run, and Runtime are not collapsed into one status indicator.
- Multi-Lane supervision is available without forcing the full cockpit on a
  user working in one Lane.

### S06 — Queue and steer a running Lane

**Starting state:** one Run is active and two prompts are queued.

**User objective:** inspect queue order, steer the active Run, reorder or remove
queued work, and verify the resulting receipt.

**Required evidence:**

- Steering and queued work are visibly different operations.
- Mutation feedback belongs to the correct Lane and Run.
- Advanced queue controls are discoverable but not permanently dominant.
- Revisions, receipts, and conflicts have user-oriented explanations.

### S07 — Review changes, tests, and approval

**Starting state:** a Run completed in isolation with a diff, test evidence, and
an approval request.

**User objective:** understand what changed, judge the evidence, approve or
reject the exact change, and see the resulting state.

**Required evidence:**

- Diff, tests, approval scope, and Run identity remain connected.
- Approval language identifies consequences and does not appear globally scoped.
- Prose, code, terminal output, and evidence have distinct visual roles.
- Destructive or irreversible outcomes are not visually equivalent to routine
  navigation.

### S08 — Recover a disconnected service

**Starting state:** a service-owned Lane loses its event stream while work state
remains durable.

**User objective:** understand whether work is still running, reconnect, and
continue from a valid event position.

**Required evidence:**

- The interface does not claim the Run stopped merely because the client
  disconnected.
- Stale/reconnecting and terminal error states are distinct.
- The primary recovery action is clear, with diagnostics behind disclosure.
- The same state is not simultaneously presented as empty.

### S09 — Resume interrupted work from a checkpoint

**Starting state:** a Run was interrupted and a verified checkpoint exists.

**User objective:** distinguish resume, retry, and start-new, then continue the
correct work deliberately.

**Required evidence:**

- Checkpoint verification and recency are understandable.
- Resume does not imply replaying an already completed Run.
- Workspace and Agent compatibility problems name a repair action.
- The resulting Run remains inside the same Lane unless the user chooses a new
  Lane.

### S10 — Archive and restore a Lane

**Starting state:** a completed Lane contains transcripts, Runs, approvals, and
evidence.

**User objective:** remove it from active work, find it later, and restore it.

**Required evidence:**

- Archive is clearly reversible and does not imply deletion.
- Preserved history is named in concise user language.
- Bulk archive is possible without encouraging Agent retirement.
- Archived results remain searchable with Agent and workspace context.

### S11 — Pause and retire an Agent

**Starting state:** a durable Agent has active and archived Lane history.

**User objective:** temporarily stop new work or deliberately retire the
identity while understanding what happens to its history.

**Required evidence:**

- Pause and Retire have different language and consequence previews.
- Retirement does not silently archive or delete Lanes.
- Active work and unresolved approvals prevent or qualify retirement as the
  contract requires.
- Historical attribution remains intact.

### S12 — Search and historical inspection

**Starting state:** many active and archived Lanes exist across several Agents.

**User objective:** find a prior decision or code change and understand which
Agent, Lane, Run, Runtime, and workspace produced it.

**Required evidence:**

- Results include durable context, not only transcript snippets.
- Active and archived status is visible.
- Scratch path names are not used as the primary identity.
- Opening a result restores a predictable navigation context.

### S13 — Narrow and keyboard-first operation

**Starting state:** the focused Lane workspace is open at a narrow desktop
width.

**User objective:** inspect status, send work, open evidence, respond to an
approval, and return to the Lane without losing context.

**Required evidence:**

- The primary Lane and composer remain understandable.
- Secondary surfaces become drawers, sheets, or explicit destinations rather
  than compressed columns.
- Focus order, visible focus, labels, and escape/close behavior are specified.
- Status is never conveyed by color alone.

## Hard gates

A direction is ineligible for recommendation if any gate fails:

1. **Explicit ownership:** composer and contextual tools identify their Lane.
2. **Lifecycle integrity:** Archive Lane and Retire Agent are distinct.
3. **State integrity:** loading, empty, disconnected, and error states cannot
   contradict one another.
4. **Runtime honesty:** local and service-owned execution are visible, and the
   design does not invent synchronization guarantees.
5. **Recovery:** disconnected and interrupted work each have a clear next
   action.
6. **History preservation:** archived work remains discoverable with Agent and
   Run context.
7. **Progressive disclosure:** expert controls remain available without making
   the default workspace a wall of equal panels.
8. **Accessible structure:** keyboard path, focus behavior, semantic labels,
   contrast, zoom/narrow layout, and reduced motion are addressed.

## Weighted scorecard

Reviewers score each criterion from 0 to 4:

- **0 — absent:** the direction does not address the criterion.
- **1 — weak:** substantial ambiguity or task failure remains.
- **2 — adequate:** the scenario is possible but requires learning or recovery.
- **3 — strong:** the design communicates the task and state clearly.
- **4 — excellent:** the design is clear, efficient, and resilient across the
  relevant contexts.

| Criterion | Weight | What the reviewer measures |
| --- | ---: | --- |
| Immediate comprehensibility | 15 | A new user can identify location, objective, status, and next action. |
| Agent/Lane lifecycle clarity | 15 | Durable identity and high-turnover work remain distinct. |
| Lane ownership and safety | 15 | Composer, tools, approvals, and evidence cannot be mistaken for another Lane. |
| Visual hierarchy and focus | 10 | Primary work dominates without hiding important attention states. |
| Local/hosted clarity | 10 | Runtime ownership, connection, and synchronization boundaries are understandable. |
| Recovery and state grammar | 10 | Empty, loading, error, disconnected, interrupted, and archived states are coherent. |
| Expert workflow preservation | 10 | Terminal, MCP, Computer Use, queue, steering, diffs, tests, and history remain usable. |
| Accessibility and narrow layout | 10 | Keyboard, focus, contrast, zoom, reduced motion, and narrow behavior are specified. |
| Migration feasibility | 5 | The direction can be delivered in vertical slices over current contracts. |

The weighted total is calculated as:

```text
sum((criterion score / 4) * criterion weight)
```

A direction must pass every hard gate and score at least 70/100 to be a viable
implementation direction. A higher score is not automatically decisive when a
lower-scoring direction contains a clearly superior solution to one critical
scenario; the decision record must explain any hybridization.

## Review protocol

1. Review each direction without seeing the other directions' scores.
2. Walk S01–S13 in order and record missing screens or transitions.
3. Evaluate all applicable Runtime contexts.
4. Mark hard gates pass/fail with a concrete prototype locator.
5. Score the weighted criteria and attach one sentence of evidence per score.
6. Record contradictions with repository facts separately from design quality.
7. Compare directions only after all independent scorecards are complete.
8. Produce one recommendation: adopt, hybridize, or reject, with migration
   slices and unresolved decisions.

## Required decision record

The final Phase 2 recommendation must include:

- selected direction or named hybrid;
- scorecards and hard-gate results;
- the scenarios where each rejected direction performed better;
- explicit local/hosted assumptions and evidence gaps;
- accessibility requirements that become implementation acceptance criteria;
- components that can be shared across the three directions;
- vertical migration slices, each with before/after evidence requirements;
- decisions deferred until a real hosted environment or Computer Use flow is
  observed.
