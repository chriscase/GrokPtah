# GrokPtah MCP control plane — coordinator guide

Loopback **MCP Streamable HTTP** surface for a separate voice-chat / orchestration
coordinator (issue #200, PR #201). Policy authority is
`OrchestrationService` only; the transport is a thin adapter.

This document is the **contract** a coordinator should implement against.
Deterministic tests that prove it live under
`crates/codegen/grokptah-agent-bridge/tests/`.

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
| Content | JSON responses preferred; GET may open a minimal SSE keep-alive |

### Health (unauthenticated, loopback)

```http
GET /health
→ 200 {"ok":true,"transport":"mcp-streamable-http","maxConcurrent":32,"sessions":N,...}
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
| `ptah_get_run` | read | `run_id` |
| `ptah_get_progress` | read | `run_id` |
| `ptah_get_events` | read | `run_id`, optional `after_seq`, `limit` (1–500) |
| `ptah_get_changes` | read | `run_id` |
| `ptah_get_test_results` | read | `run_id` |
| `ptah_get_handoff` | read | `run_id` |
| `ptah_submit_task` | mutate | `request_id`, `session_id`, `workspace`, `prompt`; optional `bounds` |
| `ptah_queue_prompt` | mutate | `request_id`, `session_id`, `workspace`, `prompt`; optional `priority` |
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
- Reads are **run-scoped** and allowlist-gated; no global event dump without `run_id`.

### Idempotency

Mutating tools take `request_id`:

- Same `request_id` + same payload → **replay** prior receipt (no double effect).
- Same `request_id` + different payload → **conflict** (fail closed).
- Safe after mid-request disconnect: retry with the same `request_id`.

### `ptah_steer` (non-cancelling)

- Guides the active Build turn at the **next safe model boundary**.
- Does **not** cancel the turn or start a second concurrent turn.
- Idle session: disposition **`queued`** (run-next / queue path).
- Active turn: disposition **`pending`** until the boundary drains it.

### Queue

- `ptah_queue_prompt` enqueues follow-ups; durable across host restart when the
  host session store reloads from the same GrokPtah home.
- Priority flag moves to front; combine rules live in host `prompt_queue`.

### Cancel

- Requires matching `session_id` + `run_id` + workspace ownership.
- Cross-session / unknown run → fail closed.
- Sets durable run state `cancelled`, tears down process tree when live.
- Idempotent on the same `request_id`.

### Run states (snake_case)

`queued` → `running` → terminal:
`completed` | `failed` | `cancelled` | `interrupted` | `limit_reached`.

### Events

- `ptah_get_events` requires `run_id`; returns `{ entries, nextCursor, cursorExpired }`.
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

## Security boundaries

| Boundary | Behavior |
|----------|----------|
| Network | Loopback only |
| Auth | Fail closed; constant-time bearer compare |
| Workspace | Allowlist + session cwd match; symlink canonicalize |
| Tools | Allowlist only; no shell/config/plugin/MCP-admin |
| Secrets | Control token scrubbed from shell env + shared event bus redaction |
| Prompts | Reject `!shell` and admin slash commands at orch validation |
| Cross-run | Cancel/read require ownership |

## Deterministic test commands

From `crates/codegen/grokptah-agent-bridge`:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings

# Focused transport + orch
cargo test --test mcp_streamable_transport -- --test-threads=1
cargo test --test orchestration_control --test orchestration_adversarial -- --test-threads=1

# Full bridge
cargo test

# Independent Node harness (server must already be up, or use Rust tests that spawn it)
# Rust entry points:
#   independent_node_mcp_sdk_interop
#   independent_node_coordinator_conformance
```

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
