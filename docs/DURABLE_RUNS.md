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

`RunBounds.maxTotalTokens` is an additive, optional contract field. When it is
omitted, model-token consumption remains unbounded as in older releases. When
it is present, the server value is a ceiling and callers may only narrow it.
GrokPtah records provider usage after every completed model response and stops
before another parent or child model request once the cumulative total meets
or exceeds the ceiling. The response that crosses the threshold is allowed to
finish, and any tool batch it already returned is completed; there is no
mid-stream or half-tool-batch interruption.

Before transmission, each attributable provider attempt is durably marked as
pending. Bounded Runs admit only one parent/child provider request at a time
and disable ambiguous transient retries; protocol-compatibility retries are
limited to rejected requests. A crash with an unresolved attempt makes usage
incomplete during recovery, so consumed work can never silently reset to zero.

Run reads expose both `aggregates.usage` and `aggregates.usageComplete`. The
latter is false when any attributable provider response omitted or malformed
its usage metadata. An unbounded run remains usable in that case but reports
partial accounting. A bounded run fails closed as `limit_reached` with typed
`stopCause: token_accounting_unavailable`; a measured threshold stop uses
`stopCause: token_ceiling`. These causes are host decisions persisted on the
run and are never inferred from model-authored prose.

Build turns create a run before model work begins. Typed bridge events are
aggregated while the turn runs and reconciled from the journal at finalization.
The store atomically installs terminal records and marks queued or running
runs `interrupted` when it is reopened. An interrupted run is inspectable but
is never resumed automatically.

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
- The existing journal, audit, idempotency, workspace allowlist, and MCP tool
  restrictions remain authoritative.
- Provider qualification and Computer Use proposal probes are not Build-run
  work and therefore are not charged to a Run ceiling. Model calls made by a
  Build parent and its spawned general-purpose children share the parent Run
  ledger. Outstanding children are cancelled and settled before a bounded Run
  is finalized so they cannot continue spending afterward.
