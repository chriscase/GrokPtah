# AGENTS.md

## Cursor Cloud specific instructions

This Cloud Agent environment is Linux. Canonical lint/test/run commands are in
[`docs/DEV_SETUP.md`](docs/DEV_SETUP.md), [`docs/VERIFICATION.md`](docs/VERIFICATION.md),
and [`docs/HEADLESS_SERVICE.md`](docs/HEADLESS_SERVICE.md).

### Four Cargo workspaces

Do **not** run `cargo test --workspace` or a root-wide `cargo test`. That path is
unsupported (vendored upstream crates are Bazel-tested). See
[`docs/VERIFICATION.md`](docs/VERIFICATION.md).

Workspaces:

- Repository root (`Cargo.toml`) — upstream TUI/CLI (`xai-grok-pager-bin`, …)
- `crates/codegen/grokptah-agent-bridge`
- `crates/codegen/grokptah-service`
- `desktop/src-tauri`

Prefer per-crate `--manifest-path` (or `cd` into that crate) with `--locked`.
Bridge and hosted-service tests must use `--test-threads=1`.

### Headless service (Linux-primary)

`grokptah-service` listens on `127.0.0.1:39200`. It requires `GROKPTAH_HOME` and
`GROKPTAH_SERVICE_TOKEN` plus at least one `--workspace`. Loopback `/health` and
`/ready` are unauthenticated; `/mcp` needs `Authorization: Bearer <token>`.
`initialize` is the only stateless MCP method and binds `mcp-session-id`. Details:
[`docs/HEADLESS_SERVICE.md`](docs/HEADLESS_SERVICE.md).

Live model calls need `XAI_API_KEY` (or another credential source in
[`README.md`](README.md)). Offline tools, crate tests, and health probes do not.
Use `GROKPTAH_AGENT_OFFLINE=1` for deterministic service tests.

Do not auto-start the service from environment `install`; start it when a task
needs the control plane.

### Desktop UI

Vite is port **1430** (`cd desktop && npm run dev`). Full Tauri
(`npm run tauri:dev`) is macOS-primary; this image does not install GTK/WebKit.
Exercise the React UI via Vite. Tauri IPC is limited without the native shell.

### protoc and pager-bin

`cargo check -p xai-grok-pager-bin` (repo root) needs `protoc` on `PATH` or
`$PROTOC`. The image installs `protobuf-compiler`. The repo also vendors
`bin/protoc` as a [dotslash](https://dotslash-cli.com) launcher; CI installs
dotslash then runs `bin/protoc --version` (see
`.github/workflows/upstream-focused.yml`).

### Linux keyring (bridge)

`grokptah-agent-bridge` links `keyring` with `sync-secret-service`. Hosted-service
CI installs `libdbus-1-dev` and `pkg-config` for that compile
(`.github/workflows/hosted-service.yml`). A live D-Bus secret service is not
assumed here; bridge tests and offline tools do not need a keychain.

### Dependency refresh

The environment `install` / Cloud Agent update script only refreshes desktop npm
deps (`npm ci` when `desktop/package-lock.json` exists). Cargo fetches on first
`cargo check` / `cargo test`. Do not put `cargo test`, `npm run dev`, or
`cargo run` in that script.
