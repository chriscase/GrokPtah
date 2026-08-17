# GrokPtah MCP control plane — coordinator guide

Loopback **MCP Streamable HTTP** surface for a separate voice-chat / orchestration
coordinator (issue #200, PR #201). Policy authority is
`OrchestrationService` only; the transport is a thin adapter.

This document is the **contract** a coordinator should implement against.
Deterministic tests that prove it live under
`crates/codegen/grokptah-agent-bridge/tests/`.

The desktop host owns the durable orchestration store. When the embedded MCP
server is enabled, it borrows that same in-process ledger, so desktop Build
runs and coordinator-submitted runs share one run namespace and restart
recovery policy. A desktop run uses `clientId: "desktop"`; coordinator runs
retain their caller identity. The MCP read tools remain run-scoped and do not
expose arbitrary desktop session history.

## Launch (desktop / production)

The Tauri desktop app starts the control plane only when a token is configured.
Bootstrap is **`start_control_from_env`** in the bridge (desktop
`start_embedded_control` is a thin wrapper — do not fork this logic).

| Env var | Required | Meaning |
|---------|----------|---------|
| `GROKPTAH_CONTROL_TOKEN` | **yes** | Bearer secret; empty/unset → control **does not start** |
| `GROKPTAH_CONTROL_PORT` | no | Bind port; default **`0`** (ephemeral). Always `127.0.0.1` |
| `GROKPTAH_CONTROL_WORKSPACES` | conditional | Platform path list (`:` on Unix, `;` on Windows). If empty, host **project cwd** is used when set; if still empty → fail closed (no server) |

### Desktop dev example

```bash
# Disposable workspace only — never point at private user trees for tests.
export GROKPTAH_CONTROL_TOKEN="$(openssl rand -hex 24)"
export GROKPTAH_CONTROL_PORT=0          # or fixed e.g. 39200
export GROKPTAH_CONTROL_WORKSPACES="/path/to/disposable/project"
# optional: offline agent for deterministic smoke
export GROKPTAH_AGENT_OFFLINE=1

cd desktop && npm run tauri:dev
# Log line on success:
#   [grokptah] MCP control plane listening on http://127.0.0.1:<port>/mcp
```

Discover the bound address from that log or `GET http://127.0.0.1:<port>/health`
(if you fixed the port). Health is **unauthenticated** but loopback-only.

### Headless / CI equivalent

Rust integration test `live_desktop_bootstrap_node_smoke` starts the **same**
bootstrap (`start_control_from_env`) with a disposable home + workspace, then
runs `tests/mcp_sdk_interop/run_live_smoke.mjs` (independent Node client).

```bash
cd crates/codegen/grokptah-agent-bridge
cargo test --test mcp_streamable_transport live_desktop_bootstrap_node_smoke -- --nocapture
```

### Soak + failure injection (coordinator hardening)

Bounded multi-session campaign (real TCP disconnects, auth/symlink/traversal
fail-closed, queue/steer/cancel, sustained polling, restart recovery):

```bash
cd crates/codegen/grokptah-agent-bridge
cargo test --test mcp_soak_hardening -- --nocapture --test-threads=1
# Node harness: tests/mcp_sdk_interop/run_soak.mjs
# Env (set by the Rust driver): GROKPTAH_MCP_URL/TOKEN/WORKSPACE/SESSION_IDS
# Optional: GROKPTAH_SOAK_SECONDS (default 25), GROKPTAH_SOAK_CONCURRENCY (default 6)
```

Crash recovery contract: reopening `OrchStore` marks unfinished runs
**`interrupted`**; session prompt queues reload from the GrokPtah home.
MCP transport sessions are **not** durable across process restart (re-`initialize`).
No model turn resumes automatically. A coordinator may use `ptah_retry_run`
with a fresh prompt to create one explicit, linked replacement after checking
the interrupted handoff.

The desktop bootstrap soak also proves a **fresh-host restart** path: it stops
the first control server, drops the original `AgentHost`, creates a new host
against the same disposable GrokPtah home, recovers the owning Build session,
and reads the prior run's state, events, and handoff through a new MCP session.
It replays a completed submission's exact `request_id` after restart and
requires the original run ID, proving the durable idempotency receipt prevents
a duplicate run.

### Durable retention

The ledger performs conservative cleanup when it opens. By default it keeps
the newest **500** safely terminal run records and expires safely terminal
records older than **30 days**. It keeps the newest **1,000** completed or
failed idempotency receipts and expires receipts older than **7 days**. These
are bounded replay windows, not a promise of indefinite request-id replay.

Retention never removes queued/running records, a run referenced by a
`retryOf` descendant, a terminal isolated run whose managed worktree still
exists, or an unrecognized/corrupt record. Pending and unknown idempotency
statuses are preserved. Coordinators that need long-term history should copy
the bounded read-tool results or handoff into their own durable store.


### Optional transport knobs (tests / soak only)

Unset in production. When set, `start_control_from_env` applies them:

| Env var | Effect |
|---------|--------|
| `GROKPTAH_CONTROL_MAX_CONCURRENT` | MCP in-flight request semaphore (default 32) |
| `GROKPTAH_CONTROL_REQUEST_TIMEOUT_MS` | Per-request wall timeout (default 120000) |
| `GROKPTAH_CONTROL_INJECT_WORK_DELAY_MS` | Hold work after permit (timeout/429 diagnostics) |

### Authentication

- Every `/mcp` request: `Authorization: Bearer <GROKPTAH_CONTROL_TOKEN>`
- Missing or wrong token → **401** (including malformed body without token —
  auth runs **before** body work)
- Token is held only in process memory / env; not written to tool outputs or shell env for agent children

### Shutdown

- Desktop process exit tears down the listener (graceful shutdown on control handle drop / cancel).
- Coordinator should `DELETE /mcp` with `mcp-session-id` to end an MCP session (**204**).
- Stale `mcp-session-id` after DELETE → client error (fail closed).
- Reconnect = new `initialize` (new session id); durable **runs** survive process restart when the same GrokPtah home + orch store is reused; in-memory MCP sessions do not.

### Optional live run events

After a run has started, a coordinator may open a scoped SSE stream on the same
MCP endpoint:

```text
GET /mcp?session_id=<Build session>&workspace=<canonical workspace>&run_id=<run>
Authorization: Bearer <token>
mcp-session-id: <transport session>
Accept: text/event-stream
```

The server first replays the durable run journal, then follows live EventBus
updates. Each event is an MCP `notifications/ptah_event` JSON-RPC notification
with `sessionId`, `workspace`, `runId`, `seq`, `ts`, and typed `update` fields.
The SSE `id` is the durable sequence and can be sent back as `Last-Event-ID`
on reconnect. The stream is authorized against the exact session/workspace/run
triple and is independently bounded to 32 concurrent streams. The unscoped GET
path remains a one-shot protocol keep-alive for clients that do not request a
run stream.

If the bounded live receiver or durable replay detects a gap, the server emits
`notifications/ptah_recovery` with `afterSeq` and `pollTool: "ptah_get_events"`
and closes the stream. Coordinators must use the cursor-based tool to recover
before reconnecting; a gap is never silently treated as a successful stream.
Runs that are still queued do not have an event range yet and return a
structured conflict. Poll `ptah_get_progress` and open the stream after
`startSeq` is present. A terminal run closes the stream after its terminal
event has been delivered.

## Transport

| Property | Value |
|----------|--------|
| Bind | **IPv4 loopback only** `127.0.0.1` (never `0.0.0.0`) |
| Paths | `POST/GET/DELETE /mcp` (Streamable HTTP); `POST /` legacy alias; `GET /health` |
| Auth | `Authorization: Bearer <token>` on every `/mcp` request |
| Auth order | Middleware authenticates **before** body work / tool dispatch |
| Body limit | **256 KiB** (`DefaultBodyLimit` + handler check) |
| Concurrent MCP requests | Default **32** → HTTP **429** + JSON-RPC error `data.code=capacity_exhausted` |
| Request timeout | Default **120 s** → HTTP **504** + `data.code=timeout` |
| MCP sessions | Header `mcp-session-id`; hard cap **256** (LRU eviction) |
| Protocol versions | `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`, `2024-10-07` |
| Content | JSON responses preferred; scoped GET may open a bounded SSE run stream |

### Health (unauthenticated, loopback)

```http
GET /health
→ 200 {"ok":true,"transport":"mcp-streamable-http","maxConcurrent":32,"maxLiveStreams":32,"sessions":N,...}
```

### Lifecycle

1. `initialize` → negotiate `protocolVersion` + capabilities; response sets `mcp-session-id`
2. `notifications/initialized` → **202** Accepted (no result body)
3. `tools/list` / `tools/call` with `mcp-session-id` (legacy clients may omit session)
4. `DELETE /mcp` with `mcp-session-id` → **204**; stale id fails closed
5. Reconnect = new `initialize` (new session id)

## Tool inventory (`CONTROL_TOOLS`)

Source of truth: `orchestration::CONTROL_TOOLS` /
`mcp_control::tool_input_schema`. Schemas use `additionalProperties: false`.

| Tool | Kind | Required arguments |
|------|------|--------------------|
| `ptah_list_sessions` | read | _(none)_ |
| `ptah_get_capacity` | read | _(none)_ |
| `ptah_get_run` | read | `session_id`, `workspace`, `run_id` |
| `ptah_get_progress` | read | `session_id`, `workspace`, `run_id` |
| `ptah_get_events` | read | `session_id`, `workspace`, `run_id`, optional `after_seq`, `limit` (1–500) |
| `ptah_get_changes` | read | `session_id`, `workspace`, `run_id` |
| `ptah_get_test_results` | read | `session_id`, `workspace`, `run_id` |
| `ptah_get_handoff` | read | `session_id`, `workspace`, `run_id` |
| `ptah_review_run` | read | `session_id`, `workspace`, `run_id` (completed isolated run only) |
| `ptah_submit_task` | mutate | `request_id`, `session_id`, `workspace`, `prompt`; optional `bounds`, `execution_mode`, `allow_queue` |
| `ptah_retry_run` | mutate | `request_id`, `session_id`, `workspace`, `run_id`, `prompt`; optional narrower `bounds`, matching `execution_mode`, `allow_queue` |
| `ptah_approve_run` | mutate | exact run/session/workspace, source and final fingerprints, exact `changed_files`; optional bounded `ttl_ms` |
| `ptah_promote_run` | mutate | `request_id`, exact run/session/workspace, `approval_id` |
| `ptah_discard_run` | mutate | `request_id`, exact run/session/workspace |
| `ptah_get_queue` | read | `session_id`, `workspace` |
| `ptah_queue_prompt` | mutate | `request_id`, `session_id`, `workspace`, `prompt`; optional `priority` |
| `ptah_edit_queue` | mutate | `request_id`, `session_id`, `workspace`, `entry_id`, `version`, `text` |
| `ptah_remove_queue` | mutate | `request_id`, `session_id`, `workspace`, `entry_id`, `expected_version` |
| `ptah_reorder_queue` | mutate | `request_id`, `session_id`, `workspace`, `entry_id`, `to_index`, `expected_version` |
| `ptah_clear_queue` | mutate | `request_id`, `session_id`, `workspace` |
| `ptah_run_next` | mutate | `request_id`, `session_id`, `workspace`, `entry_id`, `expected_version` |
| `ptah_steer_queued` | mutate | `request_id`, `session_id`, `workspace`, `entry_id`, `expected_version` |
| `ptah_steer` | mutate | `request_id`, `session_id`, `workspace`, `text` |
| `ptah_cancel` | mutate | `request_id`, `session_id`, `workspace`, `run_id` |

### Forbidden (never exposed)

`run_terminal_cmd`, `shell`, `bash`, `ptah_shell`, `ptah_set_config`,
`ptah_manage_plugin`, `ptah_manage_mcp`, `ptah_approve`, `ptah_pause`,
`ptah_resume`, `ptah_create_session`, `ptah_delete_session`.

Unknown / forbidden tool names return HTTP client error with
`error.data.code = "forbidden_scope"`.

## Semantics

### Sessions and workspaces

- Only **Build** sessions accept queue / steer / submit / cancel.
- `workspace` must be on the server **allowlist** and match the session cwd
  (canonicalized; symlink escape outside root → fail closed).
- Reads are **run-scoped and caller-scoped**: every run read and review must
  include the owning `session_id` and claimed `workspace`, which are
  canonicalized and matched against the durable run. No global event dump or
  run lookup by ID alone.

### Idempotency

Mutating tools take `request_id`:

- Same `request_id` + same payload → **replay** prior receipt (no double effect).
- Same `request_id` + different payload → **conflict** (fail closed).
- Safe after mid-request disconnect: retry with the same `request_id`.

### `ptah_retry_run` (explicit restart recovery)

- The source `run_id` must belong to the supplied Build session and allowlisted
  workspace, and its durable state must be **`interrupted`**.
- The caller must provide a fresh prompt. The original prompt is not retained
  for automatic replay, which keeps durable retention bounded and avoids
  silently repeating a partially executed task.
- The replacement preserves the source execution mode. Bounds may be omitted
  to reuse the source bounds or supplied to narrow them; server ceilings still
  apply. `allow_queue` uses the same bounded global admission scheduler.
- The new durable run exposes `retryOf` and the response exposes
  `sourceRunId`; the interrupted source record is never changed back to live.
- The mutation is idempotent and conflict-detecting on its new `request_id`.
  Cross-session, workspace-mismatched, non-interrupted, or mode-changing
  requests fail closed.

### `ptah_steer` (non-cancelling)

- Guides the active Build turn at the **next safe model boundary**.
- Does **not** cancel the turn or start a second concurrent turn.
- Idle session: disposition **`queued`** (run-next / queue path).
- Active turn: disposition **`pending`** until the boundary drains it.

### Queue

- `ptah_get_queue` returns the authenticated session's durable queued entries;
  it never returns another session's queue or a queue outside the workspace
  allowlist.
- `ptah_queue_prompt` enqueues follow-ups; durable across host restart when the
  host session store reloads from the same GrokPtah home. Its receipt includes
  `actionId`, `origin`, `action`, `disposition`, `actionVersion`, `entry`, and
  the complete post-action `entries` snapshot.
- **Every queue mutator is compare-and-set, and the version is required.**
  `ptah_edit_queue` takes the current entry `version`; `ptah_remove_queue`,
  `ptah_reorder_queue`, `ptah_run_next`, and `ptah_steer_queued` take
  `expected_version`. Omitting it is a schema rejection, not a
  last-write-wins mutation — the desktop writes this same queue, so an
  unconditional mutation is a mutation against a queue you have not read.
  A stale version is a `stale_version` conflict (HTTP 409), the queue is
  unchanged, and the fix is to re-read the queue and retry. This matches the
  Computer Use control fence, which also requires the current version on
  every transition.
- `ptah_reorder_queue` **bumps the version of every entry whose index
  changed**, including the entry it moved. `to_index` is absolute, so it only
  means something against a specific ordering; without the bump two
  coordinators could reorder concurrently, both receive success, and leave an
  arbitrary final order. Entries that did not shift keep their versions, and a
  move that lands on its current index changes nothing. Expect to refresh
  versions after any reorder, yours or someone else's.
- `ptah_run_next` promotes an entry and may explicitly cancel an active turn;
  the cancel happens only after the compare-and-set has passed, so a rejected
  call never interrupts a running turn. The cancel is also bound to the turn
  that was observed while the queue was locked: if that turn ends before the
  cancel lands, nothing is cancelled and `cancelledActive` is `false` — a
  later turn never absorbs a cancel meant for an earlier one.
- **`ptah_run_next`, `ptah_reorder_queue`, and `ptah_steer_queued` will not
  schedule an entry the control plane could not have created.** The desktop
  may author `!` shell prompts and `/` commands locally; selecting one from
  the control plane is refused with `forbidden_scope` *after* the workspace
  gate, so a cross-scope claim cannot learn that a forbidden entry exists.
  Promoting or steering that text is the same outcome
  `reject_control_prompt` exists to prevent, reached by choosing instead of
  by writing. Ordinary entries are unaffected. `ptah_steer` never cancels.
  `ptah_steer_queued` turns one queued ordinary entry into a safe-boundary
  steering action: it reports `pending` during a Build turn and `queued`
  while idle.
- `ptah_clear_queue` removes all durable queued entries for the scoped session
  **and cancels accepted steering that has not yet reached the model**. Because
  steering already handed to a model boundary cannot be retracted, an empty
  `entries` list is not on its own a promise that the session is quiet. The
  receipt reports what actually happened:
  - `clearedQueued` — durable follow-ups removed.
  - `steeringCancelled` — accepted steering stopped before injection.
  - `steeringInFlight` — steering already delivered to a boundary; it *will*
    still be injected.
  - `stopped` — `true` only when `steeringInFlight` is `0`. Branch on this,
    not on `entries` being empty.
- Every queue mutation is idempotent by `request_id`, and all mutation
  receipts use the same action identity/origin/snapshot shape so a coordinator
  can reconcile retries without guessing whether an action committed.
- Queue changes are also emitted as redacted `prompt_queue_changed` session
  events with the post-action snapshot. Delivery, deferral, and desktop
  composer consumption are journaled as state transitions, allowing a GUI or
  coordinator that reconnects to recover the same queue view without replaying
  a prompt.
- `prompt_queue_changed` carries a monotonic per-session `revision`, stamped
  under the bridge's queue mutation lock. Events are published *after* that
  lock is released, so the bus `seq` reflects publish order while `revision`
  reflects commit order. A consumer that applies snapshots must keep a
  per-session watermark and ignore any snapshot whose `revision` is not
  greater than the newest already applied; otherwise a late-published older
  snapshot silently regresses the queue. The desktop reducer does this.
- Entry `text` in `prompt_queue_changed` is **not** length-capped. Secrets are
  still scrubbed, but the text is byte-identical to what `ptah_get_queue`
  returns, so a GUI can safely seed an edit draft from the event and save it
  back. Redaction must never truncate this field.
- Priority flag moves to front; combine rules live in host `prompt_queue`.

### Bounded task admission

`ptah_submit_task` is fail-fast by default. Set `allow_queue: true` when the
coordinator wants a bounded admission queue for capacity or session contention.

- The host holds at most **32** pending task runs process-wide, even when more
  than one embedded control service shares the host. A full queue returns
  `capacity_exhausted` (HTTP 429 at the transport boundary).
- A queued response has `state: "queued"` and a one-based `queuedPosition`;
  `ptah_get_capacity` reports `queuedRuns` and `queueLimit`.
- The durable run record also exposes the current optional `queuePosition`.
  It is computed from the host-global arrival ledger, updated as earlier work
  is cancelled or admitted, cleared when the run starts, and shown by the
  desktop Task runs inspector. Reads refresh the position across embedded
  control services, so it remains meaningful when another service changes the
  queue. Treat it as live visibility, not a reservation: the run state is
  authoritative if a race is observed.
- Queued runs have durable `RunState::Queued` records and remain visible through
  `ptah_get_run`, `ptah_get_progress`, and the handoff/read tools.
- Admission uses one host-global scheduler across embedded control services.
  It preserves FIFO order for each session and prefers a different eligible
  session after a session starts, preventing one session from monopolizing the
  shared run capacity. Earlier work from the same session cannot be overtaken
  by later work from that session.
- A queued task can be cancelled with `ptah_cancel` before it starts. The
  response includes `wasQueued: true`; cancellation is idempotent and does not
  launch a model turn.
- Queue memory is process-local by design. On process restart, durable queued
  and running records are marked `interrupted`; their in-memory prompts are not
  resumed automatically. After reconnecting, a coordinator should inspect the
  durable record and use `ptah_retry_run` with a new request id only when it
  has decided that retrying is safe.

### Isolated review and promotion

- `ptah_submit_task` uses shared execution by default. A Build coordinator may
  explicitly request `execution_mode: "isolated_worktree"` for one run.
- An isolated MCP run owns one managed Git worktree and stores its execution
  metadata on the same durable run record used by the desktop. It must not
  create a duplicate desktop run.
- `ptah_review_run` returns a bounded diff, exact source/final fingerprints,
  and the exact changed-file records for a completed isolated run.
- `ptah_approve_run` persists a short-lived approval bound to the run, session,
  workspace, fingerprints, and exact changed-file set.
- `ptah_promote_run` revalidates approval expiry, ownership, fingerprints,
  current source/worktree state, managed paths, and symlinks immediately before
  applying the diff. Stale, mismatched, replayed, or cross-session approvals
  fail closed.
- `ptah_discard_run` removes a terminal isolated worktree without modifying the
  source workspace.

### Cancel

- Requires matching `session_id` + `run_id` + workspace ownership.
- Cross-session / unknown run → fail closed.
- Sets durable run state `cancelled`, tears down process tree when live.
- Idempotent on the same `request_id`.

### Run states (snake_case)

`queued` → `running` → terminal:
`completed` | `failed` | `cancelled` | `interrupted` | `limit_reached`.

### Events

- `ptah_get_events` requires the exact owning `session_id`, `workspace`, and
  `run_id`; returns `{ entries, nextCursor, cursorExpired }`. The server
  filters the bounded journal to the caller-owned run before applying `limit`,
  so activity from other sessions cannot advance the run cursor past relevant
  events.
- Sequences are monotonic for a run-scoped journal page; expired cursors return
  `cursor_expired` (HTTP 410).

### Evidence-backed handoff

`ptah_get_handoff` includes the model's final response plus bounded evidence
derived from typed bridge events. Coordinators must treat `verification.status`
as the trust signal, not the model's prose alone:

```json
{
  "verification": {
    "status": "verified | unverified | failed | incomplete",
    "stopReason": "completed | failed | cancelled | interrupted | limit_reached",
    "interrupted": false,
    "claims": {
      "present": true,
      "mentionsChanges": true,
      "mentionsTests": true,
      "mentionsVerification": true
    },
    "observations": {
      "changedFiles": 2,
      "testsObserved": 1,
      "testsPassed": 1,
      "testsFailed": 0,
      "testsIncomplete": 0,
      "permissionsRequested": 0,
      "permissionsGranted": 0,
      "permissionsDenied": 0,
      "permissionsUnresolved": 0
    },
    "usage": {
      "promptTokens": 0,
      "completionTokens": 0,
      "totalTokens": 0,
      "requests": 0
    }
  }
}
```

`changedFiles`, test outcomes, permission outcomes, interruption, and usage
are observations; claim booleans only indicate whether the final response
addressed those topics. `unverified` is expected when no test command was
observed or when the response omits claims required by the observed work.
`failed` and `incomplete` must not be reported as successful completion.

### Capacity

- Orch run capacity (`max_concurrent_runs`) is separate from MCP request
  concurrency (32). Exhausted run capacity → structured orch error
  (`capacity_exhausted` / session busy).
- MCP request flood beyond 32 inflight → **429**.

## Example client flow

```text
POST /mcp  Authorization: Bearer <token>
  initialize → session S
POST /mcp  mcp-session-id: S
  notifications/initialized → 202
POST /mcp
  tools/list → CONTROL_TOOLS only
POST /mcp
  tools/call ptah_get_capacity
POST /mcp
  tools/call ptah_submit_task { request_id, session_id, workspace, prompt }
POST /mcp
  tools/call ptah_get_run / ptah_get_events / ptah_get_handoff
POST /mcp
  tools/call ptah_steer { text }   # non-cancelling
POST /mcp
  tools/call ptah_cancel { run_id }  # explicit only
DELETE /mcp  mcp-session-id: S → 204
```

In-tree Rust helper: `McpControlClient` (`mcp_control_client.rs`).
Independent Node harness: `tests/mcp_sdk_interop/run_conformance.mjs`.

The Rust helper also exposes the coordinator-neutral live channel. After
`initialize`, construct an exact `RunScope` and call
`open_event_stream(scope, last_event_id)`. Consume
`McpEventStream::next_notification()` until it returns `None`:

```rust
let scope = RunScope { session_id, workspace, run_id };
let mut live = client.open_event_stream(scope.clone(), None).await?;
while let Some(frame) = live.next_notification().await? {
    match frame.notification {
        LiveNotification::Event(event) => observe(event),
        LiveNotification::Recovery(recovery) => {
            // Poll ptah_get_events from recovery.after_seq before reconnecting.
            recover_from_durable_events(recovery).await?;
            break;
        }
        LiveNotification::Unknown { method, .. } => record_unknown(method),
    }
}
let mut resumed = client
    .open_event_stream(scope, live.last_event_id())
    .await?;
```

The client validates the response content type, exact run scope, SSE sequence
IDs, JSON-RPC shape, and bounded frame size. It does not retry silently: a
recovery notification or transport error must be handled by the coordinator,
and the durable `ptah_get_events` tool remains authoritative.

## Security boundaries

| Boundary | Behavior |
|----------|----------|
| Network | Loopback only |
| Auth | Fail closed; constant-time bearer compare |
| Workspace | Allowlist + session cwd match; symlink canonicalize |
| Tools | Allowlist only; no shell/config/plugin/MCP-admin |
| Secrets | Control token scrubbed from shell env + shared event bus redaction |
| Prompts | Reject `!shell` and admin slash commands at orch validation |
| Cross-run | Cancel and every run read/review require exact session + workspace ownership |

## Deterministic test commands

From `crates/codegen/grokptah-agent-bridge`:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings

# Focused transport + orch
cargo test --test mcp_streamable_transport -- --test-threads=1
cargo test --test mcp_coordinator_campaign -- --test-threads=1
cargo test --test mcp_live_events -- --test-threads=1
cargo test --test orchestration_control --test orchestration_adversarial -- --test-threads=1

# Full bridge
cargo test

# Independent Node harness (server must already be up, or use Rust tests that spawn it)
# Rust entry points:
#   independent_node_mcp_sdk_interop
#   independent_node_coordinator_conformance
#   reference_coordinator_campaign_is_protocol_complete
```

### Reference coordinator campaign

`tests/mcp_sdk_interop/run_coordinator_campaign.mjs` is the reference
protocol-level coordinator workflow. It uses the platform `fetch` API so it
does not hide transport behavior behind the Rust compatibility client or an
SDK. The campaign verifies:

- bounded tool discovery, session ownership, and capacity reads;
- shared submission, durable evidence reads, idempotent replay, and cursor
  pagination with strictly increasing event sequences;
- busy-turn non-cancelling steering, explicit cancellation, idle steering, and
  queue idempotency;
- cross-session and cross-workspace mutation rejection, stale approval
  rejection, approval-scope conflicts, and isolated discard cleanup;
- isolated submission, bounded diff review, exact short-lived approval, and
  promotion into a disposable Git workspace;
- MCP session deletion/reconnect with durable run reads after reconnect.

The soak hardening suite additionally exercises a fresh-host restart against
the same durable home, including session recovery, scoped evidence reads, and
post-restart idempotent submission replay.

The Rust integration test launches the real loopback server and the Node
campaign against an offline host, so the workflow is deterministic and safe
for CI. To run the campaign against a desktop-started server, provide
`GROKPTAH_MCP_URL`, `GROKPTAH_MCP_TOKEN`, `GROKPTAH_MCP_SESSION_ID`,
`GROKPTAH_MCP_OTHER_SESSION_ID`, `GROKPTAH_MCP_DISCARD_SESSION_ID`,
`GROKPTAH_MCP_WORKSPACE`, and `GROKPTAH_MCP_DISCARD_WORKSPACE`. Both
workspaces must be explicitly disposable, allowlisted, and have clean Git
baselines. The campaign writes one named file through isolated
promotion and must never be pointed at a user's active workspace.

Optional desktop (only if desktop paths change):

```bash
cd desktop && npm run typecheck && npm test
```

## SDK limitations

- Protocol-level fetch client is the **hard gate** (`ok: true`).
- Official `@modelcontextprotocol/sdk` Streamable HTTP may still fail SSE framing
  nuances; harness reports `sdkOk` honestly and must **not** be faked.
- In-process `rmcp` is **not** linked into the bridge (reqwest 0.12 vs 0.13);
  axum implements Streamable HTTP. A quarantined rmcp crate remains optional if
  a coordinator requires byte-identical rmcp framing.

## Related

- Issue #196 — embedded control product surface
- Issue #198 / PR #199 — safety, durability, idempotency
- Issue #200 / PR #201 — standards MCP Streamable HTTP transport
- Stacked follow-up — coordinator conformance harness + docs (this guide)
