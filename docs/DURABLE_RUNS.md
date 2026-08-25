# Durable task runs

GrokPtah records Build turns as bounded durable runs under the bridge-owned
orchestration store. The desktop Tasks rail and the authenticated MCP control
plane read the same records.

## Lifecycle

```text
running -> completed
        -> failed
        -> cancelled
        -> limit_reached
        -> interrupted (only after a restart)
```

Each run records a stable run ID, session and workspace identity, a bounded
prompt preview, execution limits, journal sequence range, progress, changed
file summaries, recognized test observations, permission counts, token usage,
verification evidence, a bounded final response, and the terminal reason.

Build turns create a run before model work begins. Typed bridge events are
aggregated while the turn runs and reconciled from the journal at finalization.
The store atomically installs terminal records when it is reopened. A running
run becomes `interrupted`; a queued run keeps its accepted position when its
durable admission record survived and its receipt was settled, and otherwise
fails closed to `interrupted`. An interrupted run is inspectable but is never
resumed automatically. Every `running` run is additionally owned by an attempt
lease with a heartbeat and an expiry, so a worker that dies without finalizing
is reaped to `interrupted` with `lost_worker` rather than pinning capacity
until the next restart.

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
- Run records do not store credentials, full prompts, full transcripts, or
  unrestricted terminal output.
- Work that is accepted but not yet started is the one exception, and it is
  kept outside the run record. A private `AdmissionRecord` beside the ledger
  holds the complete bounded execution input until promotion consumes it, so a
  restart cannot destroy work the client already holds a receipt for. It is
  written `0600`, carries an integrity digest, and is never projected into a
  run, event, receipt, or capacity response — those keep the same bounded,
  redacted preview as before. See
  [durable admission and leases](./DURABLE_ADMISSION_AND_LEASES.md).
- Replaying the request ID of an accepted-but-queued run whose executable input
  did not survive a restart now returns a conflict naming that run, instead of
  replaying the original `queued` result. The run itself is `interrupted` with
  `admission_lost`. Resubmit under a new request ID.
- The existing journal, audit, idempotency, workspace allowlist, and MCP tool
  restrictions remain authoritative.
