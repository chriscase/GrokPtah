# Agent SDK read seam (contract 1.0)

`grokptah-agent-sdk` is an external, read-only Rust client for the current
GrokPtah MCP observatory tools. It is a root-workspace crate. It does not
depend on `grokptah-agent-bridge`, does not open sockets, and does not
implement authentication, TLS, or MCP session establishment.

Those concerns stay with the consumer, which supplies a small `McpTransport`
(`tools/list` + `tools/call`). `ReadObservatory::connect` projects `tools/list`
into client capabilities and then exposes:

| Method | Host tool |
| --- | --- |
| `list_sessions` | `ptah_list_sessions` |
| `list_runs` | `ptah_list_runs` |
| `observe_run` | `ptah_get_run` |
| `stream_events` | `ptah_get_events` |
| `host_capacity` | `ptah_get_capacity` |

A missing tool is `unsupported`. The SDK never invents empty listings or empty
run/event/capacity documents to cover an absent tool.

This is **not** a host-issued capability document. `computer.control` and
`provider.credentials` are permanently `Forbidden` in the client projection
even when other MCP tools appear on `tools/list`. Mutation tools, Computer Run
controls, receipts, leases, and Cursor/Claude session management are out of
scope.

## Current-main wire (not 67e29bd)

Fixtures and projections copy current `mcp_control` / `OrchestrationService`
JSON:

- Session rows: `sessionId`, `title`, `kind: "build"`, `cwd`, `workspaceStatus`,
  `updatedAt`, `busy`. Only `kind == "build"` is kept. `cwd` is stored inside
  an opaque `WorkspaceRef` and is not displayed, serialized, or returned as a
  path.
- Run records: camelCase `RunRecord` (`runId`, `sessionId`, `workspace`,
  `state`, `bounds`, `promptPreview`, `startSeq`/`endSeq`, `stopCause`,
  `aggregates.usage`, `aggregates.usageComplete`, `finalResponse`, optional
  `execution` paths). The public `RunView` keeps lifecycle, bounds, usage,
  stop cause, and event range. Prompt, final response, filesystem paths, and
  change/test/path aggregates are dropped.
- Events: `JournalPage` `{ entries: [{ seq, ts, update }], nextCursor,
  cursorExpired }`. `update` uses current `SessionUpdate` `type` tags. Public
  events keep seq/ts/kind only; text, paths, commands, tool I/O, and queue
  bodies are dropped.
- Capacity: `maxConcurrentRuns`, `activeRuns`, `available`, `queuedRuns`,
  `queueLimit`, plus `health.*Error` / `laggedLiveEvents`. Supervisor and
  native-executor objects are not forwarded.
- Errors: JSON-RPC `error.data.code` mapped one-to-one for
  `unauthenticated`, `forbidden_scope`, `workspace_mismatch`,
  `cursor_expired`, `invalid_request`, `unsupported`, `conflict`, `timeout`,
  `capacity_exhausted`, `internal`. `session_busy` and `stale_version` follow
  the host HTTP 409 class into `conflict`. Unknown codes fail closed as
  `internal`.
- Cursor expiry: host `cursor_expired` (HTTP 410). When `error.data.eventRange`
  is present (`startSeq` / `endSeq`, as on current computer-run expiry), it is
  exposed as `RetainedRange`. Build `ptah_get_events` may omit `eventRange`;
  the SDK does not invent one. `limit` is the host range **1..=500** (default
  50). `after_seq` is exclusive.

Unknown run (`invalid_request` / unknown run_id) and cross-scope run
(`forbidden_scope`) denials are collapsed to the same `forbidden_scope` so the
SDK is not a run-existence oracle. Host messages are not forwarded.

## Limitations

- Read-only. No submit, cancel, queue, steer, review, promote, or work/manager
  mutations.
- No Computer Run reads or controls (`ptah_*_computer_*` are not wired).
- No live SSE (`GET /mcp` run stream). `stream_events` is `ptah_get_events`
  paging.
- No provider credential or computer-control APIs.
- Not live/provider qualified. Consumers must still authenticate to a real
  control plane and treat this crate as a projection layer over current MCP.

Residual risk: the host Build-run path still distinguishes unknown run_id
(`invalid_request`) from cross-session (`forbidden_scope`) on the wire. The
SDK collapses those two for `observe_run` / `stream_events` only. Direct MCP
callers outside this crate still see the host distinction.
