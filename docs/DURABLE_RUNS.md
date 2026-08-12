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

The desktop inspector is read-only in this slice. Retry/continue, isolated
worktrees, diff review, and explicit promotion are later layers that must
reference a new linked attempt and preserve the original run evidence.

## Privacy and bounds

- Desktop queries are scoped to one session ID.
- Prompt previews, handoffs, progress, and errors are truncated and redacted
  through the shared event bus before persistence.
- Run records do not store credentials, full prompts, full transcripts, or
  unrestricted terminal output.
- The existing journal, audit, idempotency, workspace allowlist, and MCP tool
  restrictions remain authoritative.
