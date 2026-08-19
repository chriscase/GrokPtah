# GrokPtah headless service

`grokptah-service` is the reusable, non-desktop entry point for the GrokPtah
agent runtime. It starts the same in-process bridge and authenticated MCP
control plane used by the desktop app. Policy, persistent orchestration, and
restart recovery stay in `grokptah-agent-bridge`; the service crate owns only
configuration and process lifecycle.

## Run locally

Use a disposable workspace while testing:

```sh
export GROKPTAH_HOME=/tmp/grokptah-service-home
export GROKPTAH_SERVICE_TOKEN="$(openssl rand -hex 24)"

cargo run --manifest-path crates/codegen/grokptah-service/Cargo.toml -- \
  --workspace /path/to/project
```

The service listens on `127.0.0.1:39200` by default and prints its health URL
when ready. Set `--listen 127.0.0.1:0` for an ephemeral port in tests.

## Configuration

Command-line options override their environment equivalents:

| Option | Environment | Default |
| --- | --- | --- |
| `--listen ADDR` | `GROKPTAH_SERVICE_LISTEN` | `127.0.0.1:39200` |
| `--token TOKEN` | `GROKPTAH_SERVICE_TOKEN` | required |
| `--workspace PATH` | `GROKPTAH_SERVICE_WORKSPACES` | required; repeatable |
| `--client ID=TOKEN` | `GROKPTAH_SERVICE_CLIENTS` | primary token only; additional credentials repeatable |
| — | `GROKPTAH_SERVICE_AGENT_OWNER` | `primary` |
| `--allow-remote` | `GROKPTAH_SERVICE_ALLOW_REMOTE` | disabled |
| `--max-concurrent N` | `GROKPTAH_SERVICE_MAX_CONCURRENT` | `4` |
| `--request-timeout-ms N` | `GROKPTAH_SERVICE_REQUEST_TIMEOUT_MS` | `120000` |

`GROKPTAH_HOME` selects the durable data directory. The older
`GROKPTAH_CONTROL_TOKEN`, `GROKPTAH_CONTROL_PORT`,
`GROKPTAH_CONTROL_WORKSPACES`, `GROKPTAH_CONTROL_MAX_CONCURRENT`, and
`GROKPTAH_CONTROL_REQUEST_TIMEOUT_MS` remain accepted for migration from the
embedded desktop control plane.

Remote listeners require both `--allow-remote` and a bearer token at least 24
characters long. Health and readiness probes are authenticated when the
listener is non-loopback. The service never binds remotely by accident.

The primary credential remains compatible with `GROKPTAH_SERVICE_TOKEN`. Add
named device credentials with repeated `--client ID=TOKEN` options or a
comma-separated `GROKPTAH_SERVICE_CLIENTS` value such as
`laptop=<token>,phone=<token>`. Every credential maps to the configured
`GROKPTAH_SERVICE_AGENT_OWNER` account (default `primary`), so devices can
share durable Agent identities while Runs and audit entries retain the
credential ID that initiated them. Credentials are held in process memory and
are never written into `GROKPTAH_HOME`.

## Probes and MCP

```sh
curl http://127.0.0.1:39200/health
curl http://127.0.0.1:39200/ready
```

`/ready` returns HTTP 200 only when the durable event journal, audit ledger,
and run persistence surfaces report no active persistence error. MCP clients
connect to `/mcp` with:

```text
Authorization: Bearer <token>
```

The service exposes the same scoped orchestration tools as the desktop control
plane, including Build-session discovery/creation, task submission, durable
run history, checkpoint inspection, and explicit resume. A remote coordinator
can bootstrap a fresh allowlisted Build session with `ptah_create_session`;
session creation never accepts an arbitrary path or model policy. The service
does not resume model execution implicitly after restart. It does reconcile
durable workload leases, deadlines, dependencies, and retry admission after
startup and every five seconds while running. The latest pass is visible in
`ptah_get_capacity` under `health.workloadSupervisor`.

The current hosted boundary is intentionally single-writer: `GROKPTAH_HOME`
is a service-owned local filesystem root, whether the service runs on a laptop,
VM, or private host. Multiple clients share that one process through MCP; they
must not mount or edit the durable files directly. A database-backed,
multi-node coordinator is a later storage boundary, not an implicit property of
the current service. Durable Agents now carry the service owner account, while
frequently archived Lanes remain separate presentation/workspace projections.

## Supervised VM deployment

Linux VM operators can use the checked-in [systemd deployment](../deploy/README.md).
It runs under a dedicated `grokptah` account, stores durable state under
`/var/lib/grokptah`, and keeps the HTTP listener on loopback by default. The
unit's `ReadWritePaths` must include every path listed in
`GROKPTAH_SERVICE_WORKSPACES`.

For a remote desktop client, terminate HTTPS at a trusted reverse proxy and
forward the `/mcp` SSE stream without buffering, or carry the loopback listener
through a trusted encrypted tunnel. Setting
`GROKPTAH_SERVICE_ALLOW_REMOTE=true` only enables the process to bind a
non-loopback address; it does **not** make plaintext HTTP safe. Every
non-loopback deployment must place TLS or a trusted encrypted tunnel in front
of the service and use an explicit firewall policy. A firewall does not protect
the bearer credential in transit. Run hosted instances under a dedicated
account with systemd/container/VM policy that restricts writable paths and
process authority.

### Backup and restore

Stop the service before copying `GROKPTAH_HOME`; credentials remain in the
deployment secret/configuration and are intentionally not included in the
durable home. Restore the complete home with its ownership and permissions,
then start exactly one service against it. Never copy a live home, use a
multi-writer network filesystem, or synchronize it between active instances.
After restore, verify `/ready`, review interrupted runs, and resume a
persistent Agent only through an explicit operator action.

## Desktop remote operations

In the desktop Agents panel, connect using the service URL and bearer token.
The token is held only in the Tauri backend memory and is not persisted by the
web UI. Choose or create an allowlisted remote Build session there, then use
the composer’s **Run on** control to select the local desktop or that remote
session. Remote prompts receive a fresh request ID and enter the same durable
run ledger as service-native MCP submissions. Remote HTTP is accepted only for
loopback addresses; use HTTPS for a service reached over a network.

The desktop Task Inspector can show durable run progress from every authorized
remote Build session and replay each scoped event timeline. It lists all
durable runs, so completed and cancelled remote history remains reviewable
after the live run ends. The desktop also
opens the run-scoped SSE channel and forwards events into the inspector. If the
service or connection restarts, the watcher reinitializes MCP and resumes from
the last durable sequence. A cursor-expiry response is surfaced as a recovery
warning rather than silently skipping retained history. Steering and cancel
remain explicit, scoped MCP mutations; resuming an interrupted persistent agent
remains an operator action in the Agents panel.

## Verify

The protocol-level conformance and soak suite lives beside the service smoke
tests. It starts disposable temporary workspaces and a real loopback listener.
It never requires model credentials or outbound network.

```sh
# Full service crate: unit, smoke, and protocol conformance.
cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml

# Protocol conformance / soak only.
cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test service_conformance -- --test-threads=1
```

Desktop-facing mapping and inspector history (no service process):

```sh
cd desktop
npx vitest run src/lib/remoteExecution.test.ts src/components/RunInspector.test.tsx
npx tsc --noEmit
```

The existing desktop control-plane Node soak
(`crates/codegen/grokptah-agent-bridge/tests/mcp_sdk_interop/run_soak.mjs`)
is a different surface. Do not point it at `grokptah-service` unless you are
explicitly comparing the two listeners.

### Deterministic versus model-dependent

| Kind | What | How it is proven |
| --- | --- | --- |
| Deterministic | Auth, capacity, queue, idempotency, disconnect, restart, cursor expiry, typed receipts | `GROKPTAH_AGENT_OFFLINE=1`, temp workspaces, no API key |
| Deterministic | Desktop Local/Remote picker and submission copy | Vitest against `remoteExecution.ts` / `RunInspector` |
| Model-dependent | Agent tool quality, live model tokens, isolated-worktree promotion of real edits | Not in this suite. Use evals / a supervised VM with credentials |

Capacity tests hold an admission slot through the host instead of waiting for
a model turn. Queued work is cancelled or promoted without calling a provider.
Journal expiry is forced by flooding the in-process event bus.

### Security guarantees each scenario proves

| Scenario | Guarantee |
| --- | --- |
| Multiple authenticated MCP clients | One service process, many transport sessions, one durable ledger. A second client can see only allowlisted sessions it is authorized to read. |
| Fail-fast capacity | With `allow_queue=false` and one active slot, a second submit is rejected. It does not start a hidden turn. |
| Bounded queue | At most 32 pending admissions. The 33rd request fails closed. |
| FIFO / fairness | Queue positions follow arrival order. Same-session later work stays behind that session's older queued run. Cancelling a queued run compact the remaining positions. |
| Capacity release / promotion | Releasing the held slot promotes the FIFO head out of `queued`. |
| Idempotent replay | The same `request_id` and payload returns the original receipt (`runId` unchanged). |
| Idempotent conflict | The same `request_id` with a different payload fails closed. |
| Wrong bearer | Initialize / MCP calls with a bad token are unauthorized. |
| Unknown session | Reads against a random session id fail closed. |
| Mismatched workspace | An allowlisted workspace that is not the session cwd cannot be used as an oracle. |
| Non-allowlisted workspace | `ptah_create_session` cannot mint a session on an arbitrary path. |
| Cross-session reads / mutations | `ptah_get_run`, `ptah_get_events`, and `ptah_cancel` refuse another session's `run_id`. |
| Disconnect during request | A full-body drop still commits at most once; replay returns the receipt. A truncated body does not consume the request id. |
| MCP reconnect | `DELETE /mcp` plus a new initialize yields a new transport session; durable run reads still work. |
| Service restart | Reopening the same `GROKPTAH_HOME` exposes the same run records. |
| Cursor expiry | `ptah_get_events` and the live SSE channel fail closed on a cursor below the retained journal instead of silently skipping history. |
| Desktop contract | Create/list session shape, typed submit receipt (`runId`, `sessionId`, `state`, `requestId`, `executionMode`, optional `queuedPosition`), and `ptah_list_runs` still includes cancelled history after `current_run_id` moves. |
| Durable workloads | `ptah_list_work` / `ptah_get_work` expose the same scoped WorkItem and redacted Attempt projections used by the desktop adapter. Work mutations are idempotent, lease-token scoped, remain readable after Lane archival while archived-Lane mutations fail closed, and are reconciled by the shared supervisor after lease expiry or restart. |

The smoke tests in `tests/service_smoke.rs` remain the smaller lifecycle /
readiness / restart checks. `tests/service_conformance.rs` is the matrix above.
