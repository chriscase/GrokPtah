# GrokPtah headless service

`grokptah-service` is the reusable, non-desktop entry point for the GrokPtah
agent runtime. It starts the same in-process bridge and authenticated MCP
control plane used by the desktop app. Policy, persistent orchestration, and
restart recovery stay in `grokptah-agent-bridge`; the service crate owns only
configuration and process lifecycle.

Desktop and `grokptah-service` share a runtime; they are **not** assumed to
have identical host capabilities, and a declared capability document is not
on `origin/main`. Hosted-service CI (`.github/workflows/hosted-service.yml`)
exists only on draft [PR #352](https://github.com/chriscase/GrokPtah/pull/352)
and is **Pending — not shipped**. Stage 1 cannot pass while that PR remains
draft. **Every configured remote bearer can approve and promote within
service scope** (`ptah_approve_run`, `ptah_promote_run`). That is not
least-privilege `LocalOperator` / `RemoteCoordinator` / `Observer`
separation; those tiers must ship before any production-shaped 72-hour soak.
Shipped ManagerSupervisor is not hosted Grokbot certification.
See [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) and
[`ROADMAP_TO_100.md`](ROADMAP_TO_100.md). Always-on “Grokbot” language in
ADR-002 / [#301](https://github.com/chriscase/GrokPtah/issues/301) is not a
shipped binary name. Independent long-running workers
([#305](https://github.com/chriscase/GrokPtah/issues/305)) are a **mandatory
unmet** 100% exit and cannot be descoped; the first workload supervisor on
this service is not that exit.

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
| `--token TOKEN` | `GROKPTAH_SERVICE_TOKEN` | required; remote coordinator authority |
| `--workspace PATH` | `GROKPTAH_SERVICE_WORKSPACES` | required; repeatable |
| `--client [ROLE:]ID[/AGENT]=TOKEN` | `GROKPTAH_SERVICE_CLIENTS` | primary coordinator token plus repeatable operator/observer or Agent-bound worker credentials |
| — | `GROKPTAH_SERVICE_AGENT_OWNER` | `primary` |
| `--allow-remote` | `GROKPTAH_SERVICE_ALLOW_REMOTE` | disabled |
| `--max-concurrent N` | `GROKPTAH_SERVICE_MAX_CONCURRENT` | `4` |
| `--request-timeout-ms N` | `GROKPTAH_SERVICE_REQUEST_TIMEOUT_MS` | `120000` |

`GROKPTAH_HOME` selects the durable data directory. The older
`GROKPTAH_CONTROL_TOKEN`, `GROKPTAH_CONTROL_PORT`,
`GROKPTAH_CONTROL_WORKSPACES`, `GROKPTAH_CONTROL_MAX_CONCURRENT`, and
`GROKPTAH_CONTROL_REQUEST_TIMEOUT_MS` remain accepted for migration from the
embedded desktop control plane.

Library embedders can select the same layout explicitly with
`ServiceConfig::with_runtime_home(path)`. The service validates and
canonicalizes that root, creates the shared top-level layout, and keeps the
one-writer lock for the host lifetime. This is the current portability seam:
local desktop and hosted service use the same filesystem-backed records and
store paths, while a future database/object-store backend can replace the
layout behind the runtime-home contract without changing Agent/Lane protocol
semantics.

Remote listeners require both `--allow-remote` and a bearer token at least 24
characters long. Health and readiness probes are authenticated when the
listener is non-loopback. The service never binds remotely by accident.

The primary credential remains compatible with `GROKPTAH_SERVICE_TOKEN` and
has `coordinator` authority. Add named credentials with repeated
`--client [ROLE:]ID[/AGENT]=TOKEN` options or a comma-separated
`GROKPTAH_SERVICE_CLIENTS` value such as
`operator:laptop=<token>,observer:dashboard=<token>`. The role prefix is
optional and defaults to `coordinator`. A least-privilege worker uses
`worker:<credential-id>/<agent-id>=<token>`; it is bound to that exact Agent
and narrowed to the final configured workspace allowlist after all command-line
overrides are resolved. Every credential maps to the configured
`GROKPTAH_SERVICE_AGENT_OWNER` account (default `primary`), so devices can
share durable Agent identities while Runs and audit entries retain the
credential ID that initiated them. Credentials are held in process memory and
are never written into `GROKPTAH_HOME`.

For an operator-managed worker rotation, retain the stable credential and
Agent IDs, replace only the token in the external secret configuration, and
restart the sole-writer service. Existing MCP sessions do not survive the
process restart; the old token must fail initialization and the replacement
must open a new session. Never record either token in soak evidence.

### Authority tiers and capability discovery

Bearer authority is closed and transport-neutral. The shared service checks it
before request parsing, durable mutation, or idempotency receipt creation; it
is not inferred from a tool name, token label, desktop process, or host
filesystem permissions.

| Tier | Intended use | Key limits |
| --- | --- | --- |
| `observer` | dashboards and read-only inspection | no submission, queue, Work, Manager, routine, approval, promotion, or Computer Use mutations |
| `coordinator` | long-running agents and ordinary remote orchestration | may submit/cancel/retry Runs and coordinate Work, Managers, workers, and routines; cannot approve Work or Runs, promote/discard Runs, change managed-execution authority, or access Computer Use |
| `worker` | one independently running durable Agent | coordinator operation ceiling narrowed to one configured Agent and the service workspace allowlist; cannot impersonate another Agent, approve/promote, administer managed execution, or access Computer Use |
| `operator` | explicitly trusted remote administration | adds protected Work/Run approval, promotion/discard, and managed-execution administration; still cannot access Computer Use |
| local operator | trusted in-process desktop adapter only | never selectable by a bearer credential |

`initialize` returns the bound, secret-free capability document in
`_meta["grokptah/authorityCapabilities"]`. `tools/list` is derived from that
same document, and `ptah_get_authority_capabilities` returns it explicitly.
The document contains opaque workspace IDs, exact operation/tool lists, and
hard denials—never bearer values or canonical filesystem paths. An MCP session
is bound to the credential ID and capability-document hash used at initialize;
credential swaps or authority changes require a new session.

On the current dream candidate, that same versioned document also binds an
attempt-time host assertion. The production desktop adapter declares
`desktop_local`; `grokptah-service` declares `standalone_service`; both use an
opaque runtime-home-derived instance ID and bridge version. Common durable
capabilities are explicit, while desktop-only keychain, PTY, local approval,
and foreground semantic Computer Use are declared only by the desktop host.
Those host facts do **not** expand the connected bearer role: coordinator and
observer denials remain enforced even when the desktop host possesses a local
capability. This candidate slice is not a Stage 4 certification until its
exact-head immutable parity golden and hosted qualification pass.

`initialize` is the only stateless MCP method and the only way to create that
binding. Every later POST method—including `ping`, `tools/list`, `tools/call`,
notifications, and typed error paths—plus `GET /mcp` and `DELETE /mcp` requires
the returned `mcp-session-id`. Missing, unknown, stale, or credential-swapped
session IDs fail closed before tool dispatch or session deletion. There is no
legacy bearer-only path after initialization; clients must reinitialize after
an authority change or reconnect.

Run-scoped reads, progress, events/live streams, change and test projections,
handoffs, isolated review, and cancellation deliberately do not reveal whether
a well-formed Run ID exists outside the caller's authority. Unknown Runs,
foreign sessions, foreign workspaces, invalid session/workspace claims, and
credentials without the required workspace grant return the same typed
`forbidden_scope` code and message. A refused cancellation reaches this check
before idempotency is claimed, so it cannot leave a replayable receipt. Only a
syntactically malformed Run ID remains a distinct `invalid_request`.

Embedders may construct a credential with a narrower set of canonical
workspace roots. Authentication rejects any credential grant that exceeds the
service allowlist. The CLI currently applies the service-wide allowlist to its
named credentials; deployments needing different roots per credential should
construct `ServiceConfig::client_credentials` explicitly.

Authority-bearing idempotency receipts are namespaced by principal,
credential, grant revision, and capability-document hash; legacy receipts
without that binding fail closed. Transport audit rows record the same
non-secret authority identity. Audit persistence is currently asynchronous and
best-effort, so it is diagnostic evidence rather than a fail-closed external
side-effect authorization gate.

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

The service exposes the authority-filtered subset of scoped orchestration tools
granted to each bearer, including Build-session discovery/creation, task
submission, durable run history, checkpoint inspection, and explicit resume.
A remote coordinator
can bootstrap a fresh allowlisted Build session with `ptah_create_session`;
session creation never accepts an arbitrary path or model policy. The service
does not resume model execution implicitly after restart. It does reconcile
durable workload leases, deadlines, dependencies, and retry admission after
startup and every five seconds while running. The latest pass is visible in
`ptah_get_capacity` under `health.workloadSupervisor`. The same process is the
runtime-home owner for routine firing; `health.routineSupervisor` reports that
tick loop. Desktop clients may request create/fire/pause/enable/disable, but
they do not become the scheduler.

The current hosted boundary is intentionally single-writer: `GROKPTAH_HOME`
is a service-owned local filesystem root, whether the service runs on a laptop,
VM, or private host. Multiple clients share that one process through MCP; they
must not mount or edit the durable files directly. A database-backed,
multi-node coordinator is a later storage boundary, not an implicit property of
the current service. Durable Agents now carry the service owner account, while
frequently archived Lanes remain separate presentation/workspace projections.

**Every configured remote bearer can approve and promote within service
scope.** `--token` and each `--client ID=TOKEN` credential currently receive
the full `CONTROL_TOOLS` surface, including `ptah_approve_run` and
`ptah_promote_run`. Bearer authentication is not an Observer role. Planned
least-privilege tiers (`LocalOperator`, `RemoteCoordinator`, `Observer`) must
ship **before** any production-shaped 72-hour autonomous soak
([`ROADMAP_TO_100.md`](ROADMAP_TO_100.md) stage 3). A remote client still
cannot inherit **desktop** Computer Use, keychain, PTY, or local TCC grants
from the service host.

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
persistent Agent only through an explicit operator action. Dated
upgrade/rollback, disk-full/corrupt/torn-state, sole-writer contention,
monitoring/alerts, backup-confidentiality, and RTO/RPO drills are roadmap
stage 11; they are **not** certified by this section.

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

CI (`.github/workflows/hosted-service.yml`) formats the crate, runs clippy with
warnings denied, and executes the complete hosted-service test suite, including
standalone conformance, on locked dependencies. Manual `workflow_dispatch` is
supported. Bridge compilation in the desktop workflow is not a substitute for
these tests.

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
| Desktop contract | Create/list session shape, typed submit receipt (`runId`, `sessionId`, `state`, `requestId`, `executionMode`, optional `queuedPosition`), and `ptah_list_runs` still includes cancelled history after `current_run_id` moves. Public list/get/progress omit the frozen provider route. |
| Durable workloads | `ptah_list_work` / `ptah_get_work` expose the same scoped WorkItem and redacted Attempt projections used by the desktop adapter. Coordinator mutations (`ptah_create_work`, `ptah_assign_work`, `ptah_retry_work`, and revision-fenced `ptah_cancel_work`) are idempotent and remain separate from lease-token worker operations; `ptah_approve_work` additionally requires operator authority. Work remains readable after Lane archival while archived-Lane mutations fail closed, and the shared supervisor reconciles lease expiry or restart. |
| Durable routines | `ptah_list_routines` / `ptah_get_routine` / `ptah_list_activations` expose the same Routine and Activation records as the desktop adapter. `ptah_create_routine`, `ptah_fire_routine`, `ptah_pause_routine`, `ptah_enable_routine`, and `ptah_disable_routine` are idempotent. Manual fire creates Work through the existing workload API. The service process is the runtime-home owner that fires due schedules. |

The smoke tests in `tests/service_smoke.rs` remain the smaller lifecycle /
readiness / restart checks. `tests/service_conformance.rs` is the matrix above.
