# Agent Operations Center

The Agent Operations Center is the product-facing projection of GrokPtah's
durable runtime. It connects four durable records without collapsing them into
one ambiguous "session" concept:

| Product concept | Durable responsibility | Operator question |
| --- | --- | --- |
| Agent | Long-lived identity, policy, model, ownership, and checkpoints | Which identity is responsible, and can I explicitly resume it? |
| Lane | Build context and workspace scope; may be archived frequently | Which development context is open, archived, or unavailable? |
| Work Item | Durable objective, dependencies, retry policy, progress, and result | What work exists independently of the current UI process? |
| Run / Attempt | One bounded execution and its lease/attempt history | What is running, what happened, and what needs review? |

## Current vertical slice

The desktop `Work` view reads the same redacted Work Item and attempt shape from
both local and hosted runtimes. It keeps Lane, Agent, Runtime, Workspace, and
Run scope visible, and supports navigation to the owning Lane or linked Run.
When a hosted connection drops, the last safe snapshot remains visible and the
connection error becomes an explicit recovery state.

Local reads use `AgentHostHandle` and the existing single-owner orchestration
store. Hosted reads use the authenticated MCP service adapter. Both paths are
filtered by the durable Lane/session ID; focused UI state is not used as an
ownership lookup.

## Authority boundary

The first Work view is intentionally read-oriented. It does not expose lease
credentials, silently claim work, auto-resume an Agent, promote a Run, or widen
Computer Use permissions. Navigation actions only select the existing Lane or
Run inspection surface. Worker/service mutations remain behind their existing
authenticated, idempotent contracts until a human operator policy is designed
for them.

This preserves the runtime boundary in
[`ADR-002`](ADR-002-runtime-boundaries.md): the desktop remains the visible
authority anchor, hosted access is scoped and authenticated, and resuming an
interrupted Agent remains explicit.

## Follow-on slices

The next increments should build on this projection rather than creating a
second ledger:

1. Add an Agent detail view that groups all owned Lanes, current Work Items,
   checkpoints, connection state, and explicit lifecycle actions.
2. Add human-reviewed Work actions with clear ownership and compare-and-set
   behavior for claim, release, retry, cancel, and approval.
3. Add a durable cross-Lane timeline so an Agent's history remains understandable
   after individual Lanes are archived.
4. Exercise local desktop, local service, and hosted service flows with restart,
   reconnect, lease expiry, archival, approval, and narrow-window fixtures.
