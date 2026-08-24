# Durable memory scopes

GrokPtah addresses memory with two independent values:

1. the durable **source workspace** for a Lane; and
2. an explicit authorized scope descriptor.

The directory where a Run executes is not a memory identity. Shared Runs use
the source workspace as their execution directory, while isolated Runs use a
managed worktree. Both resolve memory through the same source workspace.

## Scope descriptors

Callers select one of these descriptors for every read or write:

```json
{ "kind": "project" }
{ "kind": "agent_private", "agent_id": "agent-..." }
{ "kind": "team", "team_id": "team-..." }
```

- `project` is visible to authorized Lanes using the same source workspace.
- `agent_private` is accepted only when `agent_id` matches the Lane's stable
  durable Agent identity. A second Agent in the same project cannot address it.
- `team` is accepted only when the caller's already-evaluated sharing policy
  explicitly approves that durable team ID. The current host approves no teams
  by default; this keeps the schema ready without creating implicit sharing.

Desktop commands identify the Lane/session and scope explicitly. Standalone
service Runs and model tools resolve the same source workspace from that
durable session record. Focused desktop state and the process's current working
directory are not memory inputs.

## Compatibility and storage

Project memory keeps the existing file and JSON format:

```text
~/.grokptah/memory/<source-workspace-hash>.json
```

Existing project facts require no rewrite or one-time migration and remain
visible after upgrade. Agent-private and team files use separate hashed paths
under `~/.grokptah/memory/scopes/<source-workspace-hash>/`; IDs are validated
and hashed before they become filenames.

The v2 hot store adds host-stamped idempotency receipts, claim keys, revisions,
compare-and-swap supersession, validity windows, surfaced conflicting heads,
critical-fact protection, and bounded compaction. Retrieval excludes expired
and superseded facts from the current view, reports unresolved conflicts, and
keeps each scope independent. The exact ceilings remain enforced for facts,
fact/tag/query sizes, receipts, persisted bytes, critical bytes, files, scope
footprint, and the 6,000-byte injected project context.

## Manager occurrence attribution

An autonomous manager decision captures project memory once, under the exact
AgentSpec revision active when the occurrence is created. The durable
attribution binds the source workspace, the complete canonical memory policy,
the exact quoted context and its byte count, and the decision Work objective.
The proposal Run receives no second ambient memory injection and has no tool
authority, so it cannot read agent-private/team memory or observe later project
facts. Objective, AgentSpec, policy, context, or directive-digest drift fails
closed before it can mutate a manager plan. See
[`MANAGER_PLANS.md`](MANAGER_PLANS.md#durable-manager-decisions).

## Promotion, discard, and retention

A successful memory tool write commits durable host state immediately. It is
not part of an isolated Git worktree:

- **Promote** applies reviewed source changes but does not move, duplicate, or
  delete memory.
- **Discard** removes the managed execution worktree but does not roll back a
  completed memory write.
- **Restart** reopens memory from the same GrokPtah home and source-workspace
  identity.

If a future workflow needs transactional memory that rolls back with code, it
must introduce a separate staged-memory protocol. Worktree cleanup must never
silently approximate that behavior.
