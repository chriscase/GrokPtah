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

The Work view exposes only human-reviewed lifecycle actions: create, assign,
retry a failed item within its declared budget, approve an approval-gated
completion, and cancel. It does not expose lease credentials, silently claim
work, auto-resume an Agent, promote a Run, or widen Computer Use permissions.
Worker actions such as claim, renew, progress, release, complete, and fail
remain behind the authenticated MCP contract. Revision fences make stale
desktop decisions fail closed, and archived Lanes remain inspection-only.

This preserves the runtime boundary in
[`ADR-002`](ADR-002-runtime-boundaries.md): the desktop remains the visible
authority anchor, hosted access is scoped and authenticated, and resuming an
interrupted Agent remains explicit.

## Follow-on slices

The next increments should build on this projection rather than creating a
second ledger:

1. Add an Agent detail view that groups all owned Lanes, current Work Items,
   checkpoints, connection state, and explicit lifecycle actions.
2. Add worker/coordinator scheduling policy around the human-reviewed Work
   actions, including explicit assignment discovery and claim/release policy.
3. Add a durable cross-Lane timeline so an Agent's history remains understandable
   after individual Lanes are archived.
4. Exercise local desktop, local service, and hosted service flows with restart,
   reconnect, lease expiry, archival, approval, and narrow-window fixtures.
5. Keep the Routines panel request-only: create, inspect, manual fire, pause,
   enable, and disable. The runtime-home owner remains the only scheduler. See
   [DURABLE_ROUTINES.md](DURABLE_ROUTINES.md).
