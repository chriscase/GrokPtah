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

## Provider-send journal

A run is marked `running` before any provider request exists, and one logical
model step can issue several *physical* provider requests: a credential
refresh after HTTP 401, a transport or 429/5xx retry, a `tool_choice`
fallback, and a non-stream fallback. Each physical request is therefore
recorded as its own durable attempt under the orchestration store, beside the
run records, so a crash or a cancel never has to guess whether the provider
already executed the work.

```text
known_not_sent -> sending -> sent -> responding -> settled
       |            |         |          |
       |            +---------+----------+-> uncertain -(reconcile)-> settled
       |
       +-(reopen: nothing left the process)-> settled (not_sent)
```

| State | Meaning | Reopen result |
| --- | --- | --- |
| `known_not_sent` | Intent is durable; no byte reached the transport | `settled` / `not_sent` |
| `sending` | The physical send is in flight | `uncertain` |
| `sent` | The provider proved receipt by producing a response head | `uncertain` |
| `responding` | The response is being consumed | `uncertain` |
| `uncertain` | Outcome unknown; remote work may be unresolved | `uncertain` (preserved) |
| `settled` | Outcome durably known (`not_sent`, `accepted`, `provider_rejected`) | unchanged |

Each attempt binds the run, the model-step round, a strictly increasing
physical ordinal, the request and body digests, the route identity, the
provider profile and dialect, the wire model, and the credential revision.
Every authorized resend allocates a new ordinal and a new request digest; an
ordinal is one physical request and is never reused. Route identity for a
private compatible gateway is an opaque, stable label, and the credential
revision is a digest of the credential's identity — never the credential.

The rules the journal enforces:

- The `known_not_sent` record is durable *before* the physical write, so a
  crash observed there proves nothing left the process.
- Any error after the physical-send boundary becomes `uncertain`. The only
  exception is a connect-phase failure, which proves the request never
  reached the provider and settles as `not_sent`.
- `uncertain` is sticky. Reopening the store any number of times preserves it,
  and a later failure never overwrites the reason that first fenced it.
- A journal entry this process cannot read or whose binding digest does not
  verify is treated as unresolved work, not absent work.

While a run has an unresolved attempt:

- automatic resend inside the model helper stops instead of looping;
- explicit `ptah_retry_run` is refused with `conflict`, naming the outstanding
  attempts;
- the run's admission slot is deliberately held after the turn ends —
  including after cancellation — so a replacement run cannot overlap work the
  provider may still be doing.

Reconciliation is the only exit. It must re-present the exact request digest
and the credential revision the attempt was issued under, so a proof that
belongs to a different request or a rotated credential is refused, and an
attempt that is already settled cannot be reconciled twice. Clearing the last
outstanding attempt is what returns the held admission slot. A restart clears
the in-process capacity hold, but the durable journal keeps fencing retry
until the attempt is reconciled.

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
