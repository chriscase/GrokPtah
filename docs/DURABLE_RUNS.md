# Durable task runs

GrokPtah records Build turns as bounded durable runs under the bridge-owned
orchestration store. The desktop Tasks rail and the authenticated MCP control
plane read the same records.

## Lifecycle

```text
queued  -> running
        -> cancelled
        -> interrupted (admission_lost / admission_tampered)
        -> failed     (admission could not be completed)
running -> completed
        -> failed
        -> cancelled
        -> limit_reached
        -> interrupted
```

`interrupted` is produced by two different mechanisms, and both are terminal:

- the **live reaper**, when a running attempt is torn down while the process is
  still up — an expired attempt lease, a lost lease, shutdown, or an outer
  supervisor that exited without installing its own terminal record; and
- the **restart sweep**, when the process died with the run still `running`.

Every accepted task is written as `queued` first, even one that will execute
immediately, and only reaches `running` once it holds its attempt lease.

Each run records a stable run ID, session and workspace identity, a bounded
prompt preview, execution limits, journal sequence range, progress, changed
file summaries, recognized test observations, permission counts, token usage,
verification evidence, a bounded final response, and the terminal reason.

Build turns create a run before model work begins. Typed bridge events are
aggregated while the turn runs and reconciled from the journal at finalization.
The store atomically installs terminal records when it is reopened.

Restart recovery treats the two non-terminal states differently:

- A **`running`** run is always terminalized `interrupted`. Model work is never
  resumed implicitly after a restart: the transcript position, the tool state,
  and the provider stream are all gone, so continuing would be a guess. An
  interrupted run is inspectable, and `ptah_retry_run` is the only way to carry
  its work forward — as a new, separately idempotent request.
- A **`queued`** run is re-admitted and executed, exactly once, provided its
  admission is provably complete: a verifying sealed acceptance intent *and* a
  completed idempotency receipt naming that exact run. Anything else is
  tombstoned `interrupted` with `admission_lost` or `admission_tampered` and
  can never execute.

See [Durable admission and attempt leases](./DURABLE_ADMISSION_AND_LEASES.md)
for the crash-safe cuts this depends on.

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
  unrestricted terminal output. The full prompt exists in exactly one durable
  place — the private, owner-only acceptance intent — and is destroyed when the
  run becomes terminal.
- Idempotency receipts carry the accept response, never the accepted input.
- The existing journal, audit, idempotency, workspace allowlist, and MCP tool
  restrictions remain authoritative.
