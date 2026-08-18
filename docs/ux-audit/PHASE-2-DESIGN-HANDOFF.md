# GrokPtah Phase 2 design handoff

Purpose: give a design-capable Claude or Cursor model a precise brief for
prototyping the next GrokPtah experience after the Phase 1 UX audit.

Related artifacts:

- [Phase 1 UX audit](PHASE-1-UX-AUDIT.md)
- [Agent–Lane–Run runtime model](../AGENT_LANE_RUNTIME_MODEL.md)
- [Persistent agent protocol](../PERSISTENT_AGENT_PROTOCOL.md)
- [Headless service contract](../HEADLESS_SERVICE.md)

## Brief

Design a local-first and hosted-capable GrokPtah workspace around two distinct
user concepts:

1. Durable **Agents** that persist as identities, and
2. frequently archived **Lanes** that represent individual work contexts.

An Agent may own many Lanes. A Lane may be ad hoc without an Agent. Each Lane
contains many durable Runs and can execute locally, through a local service/VM,
or through a hosted service. Archiving a Lane hides it from active work but
preserves its transcript, Runs, checkpoints, approvals, evidence, artifacts,
and history. Retiring an Agent is separate from archiving any Lane.

Do not redesign around the current “103 sessions plus many equal panels” model.
Use Phase 1 evidence to reduce cognitive load while retaining expert access to
queues, steering, terminals, diffs, tests, approvals, MCP, Computer Use,
worktrees, and task history.

## Required prototype surfaces

### 1. Agent home

Show a roster of durable Agents, each with:

- name and role;
- operational health and lifecycle;
- current Runtime target and connection state;
- active Lane count and archived Lane count;
- current Lane, if any;
- last checkpoint or “no checkpoint yet”;
- Create Agent, Pause, Retire, and Open details actions.

The empty state must distinguish “no Agents exist” from “Agents could not be
loaded.”

### 2. Agent detail

Show the Agent’s identity and memory/policy summary, then its Lanes grouped as
Active, Attention needed, and Archived. Make the one-Agent-to-many-Lanes
relationship visible without requiring a technical diagram.

The user should be able to start a new Lane from this page, choose a Runtime
target, and optionally assign an existing Agent to an ad-hoc Lane.

### 3. Lane list and Archive

Show Lanes as work records, not anonymous sessions. Each row/card should include:

- concise title and objective;
- Agent name or “Ad hoc”;
- project/workspace display name;
- Runtime target and connection state;
- current Lane status and next action;
- last activity;
- current Run, queue, approval, diff, and test indicators;
- archive/restore actions with clear reversible semantics.

Provide Active, Attention, Archived, and All views. Search should return Lane
identity and Agent context alongside matched snippets.

### 4. Focused Lane workspace

Create one dominant active Lane surface with:

- a persistent context header: Lane, Agent, Runtime, Workspace, Run;
- transcript/progress as the primary content;
- one composer whose target is explicit;
- contextual drawers for Queue/Steering, Tools, Terminal, Diff/Tests,
  Approvals, MCP, Computer Use, and Run history;
- a single clear next action for blocked, interrupted, disconnected, missing,
  or awaiting-approval states.

Multiple lanes may be opened beside one another for expert users, but every
zone and every contextual drawer must show its own Lane scope.

### 5. Runtime target and connection state

Design a visible target selector with:

- Local desktop;
- Local service/VM;
- Hosted service.

Show what is local versus service-owned, whether the workspace is allowlisted,
whether the target is connected, and what is synchronized. Do not imply that
credentials or source files sync unless the product contract explicitly says so.

### 6. Error and recovery states

Prototype at least these states:

- no Agents yet;
- no active Lanes;
- refresh failed because the durable store is unavailable;
- missing local workspace;
- disconnected remote service;
- stale/reconnecting event stream;
- queued Run;
- awaiting approval for an isolated diff;
- interrupted Run with a verified checkpoint;
- archived Lane;
- retired Agent.

Primary copy must be user-oriented. Raw paths, bridge versions, transport
details, and OS error numbers belong behind Technical details or an exportable
diagnostic record.

## Suggested information architecture

```text
GrokPtah
├── Agents
│   ├── Active identities
│   ├── Needs attention
│   ├── Paused
│   └── Retired
├── Lanes
│   ├── Active
│   ├── Attention needed
│   ├── Archived
│   └── All
├── Focused Lane
│   ├── Transcript / progress
│   ├── Queue & steering
│   ├── Changes & tests
│   ├── Approvals
│   ├── Terminal
│   ├── MCP / tools
│   ├── Computer Run
│   └── Run history / checkpoints
└── Settings
    ├── Runtime targets
    ├── Permissions & safety
    ├── Providers & authentication
    └── Appearance
```

This is a product-level IA proposal. It does not require deleting the existing
Sessions view immediately; a compatibility view can show existing sessions as
Lanes while the new model is introduced.

## Interaction rules

- A new Lane can be started without creating a Durable Agent.
- Assigning an Agent to a Lane is explicit and visible in the Lane header.
- An Agent can own many active and archived Lanes.
- Archive is reversible and does not delete work history.
- Resume is always explicit and should say whether it resumes from a verified
  checkpoint, retries an interrupted Run, or starts a new Run.
- A Lane cannot execute when its workspace is missing or its Runtime target is
  disconnected; the UI must name the repair action.
- Approvals belong to a specific Run and changed-file fingerprint.
- Tools, Git, Terminal, Queue, Steering, Computer Run, and MCP never infer
  ownership from the global focused tab; they receive an explicit Lane scope.
- Switching Runtime target is visible before the prompt is submitted.
- Remote tokens remain hidden and are not shown in status text.

## Acceptance criteria for the design prototype

The prototype is ready for implementation review when a reviewer can answer
these questions without reading a technical manual:

1. Which durable identity am I working with?
2. Which Lane is active, and what is its objective?
3. Which Runtime target will execute the next prompt?
4. What workspace and branch/worktree can this Lane touch?
5. Is the Agent running, waiting, blocked, disconnected, or awaiting approval?
6. What changed, what tests ran, and what requires my decision?
7. What will Archive preserve, and how do I restore the Lane?
8. What survives if the desktop closes or the hosted service reconnects?

## Handoff prompt for Claude/Cursor

```text
You are designing the next GrokPtah information architecture after a real
Phase 1 product-research audit. Do not implement production code yet.

Read:
- docs/ux-audit/PHASE-1-UX-AUDIT.md
- docs/AGENT_LANE_RUNTIME_MODEL.md
- docs/PERSISTENT_AGENT_PROTOCOL.md
- docs/HEADLESS_SERVICE.md

Produce a design proposal for a local-first and hosted-capable coding-agent
workspace built around:

- Durable Agents: long-lived identities, role, policy, memory/checkpoints,
  lifecycle, and health.
- Lanes: high-turnover work contexts with objective, workspace, transcript,
  queue, steering, changes, tests, approvals, and runtime target.
- Runs: durable executions inside a Lane, with progress, interruption,
  checkpoint, retry, diff, test, and promotion evidence.

One Agent must be able to own many Lanes. A Lane may be ad hoc without an
Agent. Archiving a Lane must preserve its transcript, Runs, checkpoints,
approvals, evidence, artifacts, and history; retiring an Agent is separate.

Design these surfaces:
1. Agent roster/home;
2. Agent detail with active and archived Lanes;
3. Lane list/archive/search;
4. one focused Lane workspace with contextual drawers;
5. local desktop / local service / hosted service target selection;
6. error and recovery states for missing workspace, disconnected service,
   store refresh failure, interrupted Run, queued Run, approval, archive, and
   retirement.

Use progressive disclosure. The default experience should have one clear Lane
and composer; Live, Tools, terminal, Computer Run, MCP, queue, steering,
worktrees, diffs, tests, and task history remain available as contextual
surfaces for expert users.

Deliver:
- an annotated information architecture;
- low-fidelity wireframes or a clickable static prototype;
- a state/transition matrix;
- explicit local-vs-hosted behavior and synchronization boundaries;
- migration notes that preserve the existing session/transcript and durable
  run contracts;
- a short list of implementation slices, without editing production code.

Separate observed facts from recommendations. Do not invent successful hosted
or Computer Use behavior that the audit did not observe.
```

## Recommended implementation sequence after design review

1. Add a read-only Lane projection over existing `SessionSummary` records.
2. Make panel/composer/tool requests explicitly Lane-scoped.
3. Decouple persistent Agent identity from one `session_id` while preserving
   compatibility with legacy records.
4. Add a normalized runtime-target and connection projection.
5. Normalize storage/bridge errors into the user-facing state model.
6. Implement the new navigation and focused workspace in small UI PRs.

