# GrokPtah Architecture

GrokPtah is a fork of [xai-org/grok-build](https://github.com/xai-org/grok-build)
that keeps the upstream CLI/TUI and adds a Tauri desktop plus a standalone
service over one shared agent runtime. The desktop can host that runtime
locally or connect to one authoritative local/private-cloud agent home.

## Locked decisions

| Decision | Choice |
|----------|--------|
| Agent bridge | **In-process** — no `grok agent stdio` child on the happy path |
| UI | React + Vite, dark desktop chrome (GrokPtah branding only) |
| Scope | TUI **capability** parity (not pixel-perfect ratatui) |
| CLI | `xai-grok-pager-bin` remains independently buildable |
| Execution hosts | Tauri desktop and `grokptah-service` over the same bridge/runtime contract |
| Durable home | One owning process and one authoritative `GROKPTAH_HOME`; remote devices are clients, not filesystem writers |
| Build agent runtime | **Hybrid thin loop** deepened in-bridge; path to embed upstream — see [`ADR-001-agent-runtime.md`](./ADR-001-agent-runtime.md) and [`ADR-002-runtime-boundaries.md`](./ADR-002-runtime-boundaries.md) |

## Layer diagram

```
┌──────────────────────────────────────────────────────────┐
│ Clients                                                  │
│ React desktop · MCP coordinators · future web/mobile     │
└──────────────────────────┬───────────────────────────────┘
                           │ typed IPC or authenticated MCP/API
┌──────────────────────────▼───────────────────────────────┐
│ Hosts                                                    │
│ desktop/src-tauri (dialogs, PTY, OS grants)              │
│ grokptah-service (configured roots, listener, readiness) │
└──────────────────────────┬───────────────────────────────┘
                           │ shared in-process domain API
┌──────────────────────────▼───────────────────────────────┐
│ grokptah-agent-bridge                                    │
│ sessions · finite runs · agents · memory · policy        │
│ durable events · workloads · routines · isolation/review │
└──────────────────────────┬───────────────────────────────┘
                           │ provider/profile + tool integrations
┌──────────────────────────▼───────────────────────────────┐
│ Upstream crates (CLI path unchanged)                     │
│ xai-grok-shell · tools · workspace · auth · pager TUI    │
└──────────────────────────────────────────────────────────┘
```

The `crates/codegen/grokptah-service` binary starts the same bridge and
authenticated MCP control plane without Tauri. It is the headless **host** for
local service, VM, or private-cloud deployments; the desktop remains the
primary interactive client and may also be a local host. Protocol-level
remote-service conformance (multi-client, capacity, idempotency,
authorization, reconnect, desktop receipts) is documented in
[`HEADLESS_SERVICE.md`](./HEADLESS_SERVICE.md). Runtime, storage, capability,
and authority boundaries are normative in
[`ADR-002-runtime-boundaries.md`](./ADR-002-runtime-boundaries.md). The
host-neutral embedding surface another product links against — four bounded
operations over the same runtime, with redacted projections — is
[`HEADLESS_AGENT_PORT.md`](./HEADLESS_AGENT_PORT.md).

## Why a bridge crate

Upstream already separates **agent** (`xai-grok-shell`, ACP) from **TUI**
(`xai-grok-pager`). Desktop is a new client. Public shell entry points
(`run_stdio_agent`) are stdio-bound; the bridge owns an in-process
ACP-shaped host so the UI never spawns a second agent process.

The bridge:

- Owns agent start/stop and project cwd
- Creates/loads sessions
- Accepts prompts, streams `message` / `thought` / `tool_call` / `tool_call_update` / `plan`
- Completes permission requests from the UI
- Supports cancel, fork, rewind, compact, sessions list
- Owns one durable Build-run ledger shared by the desktop and MCP coordinator
  surfaces; unfinished runs become `interrupted` on restart and never resume
  model execution implicitly
- Supports opt-in isolated Build worktrees with bounded diff review, source
  fingerprint checks, explicit promotion, and managed discard
- Runs **local tools** (read/list/grep, shell, write with permission) in-process
- **Build sessions** run a multi-round **tool-calling agent loop** (list/read/grep/glob/write/apply_patch/shell) with permissions; **Chat sessions** are single-shot completions
- Injects project instructions (`AGENTS.md`, etc.) into Build context; sends effort on the wire
- When OIDC/`~/.grok/auth.json` or an xAI API key is present, calls cli-chat-proxy / API for model steps
- Discovers MCP servers from `~/.grokptah/mcp.json` / project `.mcp.json`, skills under `~/.grokptah/skills` and project skill dirs, plugins under `~/.grokptah/plugins` + local catalog (MCP **dispatch** into the loop is Phase 15)
- Background tasks run real async work (directory walk) via `tokio::spawn`
- Integrated terminal PTYs forward stdout to the UI (`pty://output`) with multi-tab backlog replay

## Workspace layout

| Path | Role |
|------|------|
| `desktop/` | Nested npm + Tauri app (own Cargo workspace under `src-tauri`) |
| `crates/codegen/grokptah-agent-bridge` | Protocol host used by Tauri (also in nested workspace) |
| `crates/codegen/grokptah-service` | Standalone headless service entry point (own Cargo workspace) |
| `crates/codegen/xai-grok-*` | Upstream CLI/TUI closure (root workspace; treat root `Cargo.toml` as generated) |

Root `Cargo.toml` is auto-generated by upstream. Desktop uses a **nested**
Cargo workspace so we do not require regenerating the monorepo root.

## Local and hosted agent homes

The desktop can host a local home or connect to the standalone service running
locally or behind TLS on a private host. One process owns each
`GROKPTAH_HOME`; all other devices are protocol clients of that owner.
The remote path reuses the scoped MCP contract: persistent-agent runs are
discovered from the allowlisted service, durable event pages provide cursor
catch-up, and the run-scoped live channel provides low-latency updates. Tauri
holds the bearer token in backend memory and owns reconnect/session recovery;
the React layer receives only typed run events and recovery notices.

For non-loopback use, TLS, a trusted encrypted tunnel, or a trusted TLS
terminator is mandatory, and the service runs under a dedicated account with
host-level filesystem/process confinement. The workspace allowlist selects
exact authorized project identities; it is not an OS sandbox. The current
single service bearer is operator-equivalent for the full MCP surface,
including isolated-run approval and promotion; scoped principal tiers are a
required boundary before durable worker leases ship.

```sh
# CLI / TUI (root workspace)
cargo run -p xai-grok-pager-bin

# Desktop
cd desktop && npm install && npm run tauri dev
```

## Non-goals (intermediate)

- Windows/Linux first-class packaging (macOS primary)
- Production notarization / App Store
- Rewriting shell/tools/workspace
- Upstream PRs to xai-org/grok-build

## Auto-update

Desktop builds disable upstream xAI CLI auto-update prompts so users are not
directed to replace GrokPtah with the official CLI binary.
