# Durable task runs

GrokPtah records Build turns as bounded durable runs under the bridge-owned
orchestration store. The desktop Tasks rail and the authenticated MCP control
plane read the same records.

## Lifecycle

```text
accepted -> queued -> running
                    |
running -> completed
        -> failed
        -> cancelled
        -> limit_reached
        -> interrupted (after restart or live lease expiry)
```

Each run records a stable run ID, session and workspace identity, a bounded
prompt preview, execution limits, journal sequence range, progress, changed
file summaries, recognized test observations, permission counts, token usage,
verification evidence, a bounded final response, and the terminal reason.

Build turns create a run before model work begins. Typed bridge events are
aggregated while the turn runs and reconciled from the journal at finalization.
The store atomically installs terminal records. Accepted `queued` runs retain
their full input and host admission identity in the private acceptance ledger;
reopening reconstructs their FIFO/host slots and resumes dispatch exactly once.
Model work that had reached `running` is marked `interrupted` when it is
reopened. An interrupted run is inspectable but is never resumed automatically:
it requires an explicit retry with a fresh prompt.

Each running model attempt also has a private CAS lease with an owner,
revision, heartbeat, expiry, and phase. A periodic reaper can interrupt only
the exact expired attempt. Finalization retries are bounded; an unresolved
terminal write remains in the bounded recovery queue and projects degraded
durability health rather than wedging capacity.
Live lease expiry is a liveness terminal transition, not a restart-only
condition; the expired worker is stopped and awaited before capacity is reused.

An admission or receipt write error is a failed acceptance: its private input is
tombstoned, no model attempt is started, and a later restart cannot recover it.
If a settled receipt names queued work whose acceptance record is gone, replay
fails closed with `admission_lost` rather than returning stale queued success.

The desktop inspector is read-only for shared runs. Build sessions may opt into
strict isolated execution. An isolated run starts from a clean Git workspace in
a detached worktree below `.grokptah/worktrees/runs/`; the model writes only to
that worktree. A completed run records its final fingerprint and changed-file
summary as `ready` only after snapshot verification.

Promotion is deliberately explicit and review-gated:

1. Select `Isolated` for a Build session before starting a turn.
2. Open the Tasks rail and choose `Review diff` on a completed run.
3. Promote is enabled only while the review fingerprint still matches the
   durable run record.
4. Promotion checks that the original source workspace is unchanged, validates
   relative paths and symlink boundaries, applies the bounded Git patch, and
   verifies the final fingerprint. Repeating the same request is idempotent.
5. `Discard` removes only the managed run worktree. It never edits the source
   workspace.

Dirty source workspaces, source changes during execution, changed isolated
worktrees, protected metadata paths, and promotion conflicts fail closed. MCP
coordinators can review, approve, promote, or discard an isolated run through
the bounded control tools. Approval is short-lived and bound to the run,
session, workspace, source and final fingerprints, and exact changed-file set;
the promotion path revalidates all of those constraints immediately before
applying the change.

## Coordinator visibility

MCP-submitted runs are recorded with `clientId: "mcp"`, while desktop turns use
`clientId: "desktop"`. When a coordinator is driving a Build session, the
existing session tab and optional Live rail show an `MCP` badge, and the Tasks
rail labels the durable run `MCP coordinator`. The same label is hydrated when
switching sessions or reloading while a run is still active, so the desktop
does not need a second event stream or a separate coordinator dashboard.

## Privacy and bounds

- Desktop queries are scoped to one session ID.
- Prompt previews, handoffs, progress, and errors are truncated and redacted
  through the shared event bus before persistence.
- Public run records do not store credentials, full prompts, full transcripts,
  or unrestricted terminal output. Full accepted input exists only in the
  bridge-owned private 0600 acceptance ledger and is tombstoned before a model
  attempt is dispatched or a queued run is cancelled.
- The existing journal, audit, idempotency, workspace allowlist, and MCP tool
  restrictions remain authoritative.
