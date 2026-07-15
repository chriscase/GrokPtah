# GrokPtah

**GrokPtah** is a fork of [Grok Build](https://github.com/xai-org/grok-build)
(SpaceXAI / xAI coding agent) focused on a **Tauri desktop** client with
**in-process** agent runtime, while preserving the upstream CLI/TUI.

| | |
|--|--|
| Upstream | [xai-org/grok-build](https://github.com/xai-org/grok-build) |
| License | Apache License 2.0 (see [`LICENSE`](LICENSE)) |
| Milestone | [Tauri Desktop — TUI Feature Parity](https://github.com/chriscase/GrokPtah/milestone/1) |
| Architecture | [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) |
| Setup | [`docs/DEV_SETUP.md`](docs/DEV_SETUP.md) |

---

## Clients

### Desktop (primary product for this fork)

```sh
cd desktop
npm install
npm run tauri dev
```

React + Vite UI in a Tauri 2 window. The agent runs **in-process** via
`grokptah-agent-bridge` (no second `grok agent stdio` process on the happy path).

Release / package (macOS, unsigned OK):

```sh
cd desktop && npm run tauri build
```

### CLI / TUI (upstream path)

```sh
cargo run -p xai-grok-pager-bin              # build + launch TUI
cargo build -p xai-grok-pager-bin --release  # target/release/xai-grok-pager
cargo check -p xai-grok-pager-bin
```

Requirements: Rust (see `rust-toolchain.toml`), `protoc` on `PATH`.
Official installs ship the binary as `grok`; this tree builds `xai-grok-pager`.

---

## Repository layout

| Path | Contents |
|------|----------|
| `desktop/` | Tauri 2 + React/Vite GrokPtah app |
| `crates/codegen/grokptah-agent-bridge` | In-process agent host + protocol |
| `crates/codegen/xai-grok-pager*` | Upstream TUI |
| `crates/codegen/xai-grok-shell` | Upstream agent runtime |
| `docs/` | Architecture + dev setup |

> Root `Cargo.toml` is **generated** by upstream — treat as read-only.
> Desktop uses a nested Cargo workspace under `desktop/src-tauri`.

---

## Development

```sh
# Desktop
cd desktop && npm run typecheck && npm test
cd desktop/src-tauri && cargo test && cargo check

# CLI
cargo check -p xai-grok-pager-bin
cargo test -p grokptah-agent-bridge   # if linked from root (prefer nested workspace)
```

Upstream-style checks:

```sh
cargo check -p <crate>
cargo clippy -p <crate>
cargo fmt --all
```

---

## Auto-update

GrokPtah **desktop** does not run the upstream xAI CLI auto-updater, so the
desktop build will not prompt to replace itself with the official `grok` CLI.

---

## Contributing / security

This fork is developed for GrokPtah desktop work. Upstream
[`CONTRIBUTING.md`](CONTRIBUTING.md) notes that xAI does not accept external
PRs on the original tree. Security: see [`SECURITY.md`](SECURITY.md).

## License

First-party code is **Apache-2.0**. Third-party notices: `THIRD-PARTY-NOTICES`,
`third_party/NOTICE`, and crate-local notices under `crates/codegen/`.

Original Grok Build © SpaceXAI / xAI contributors.
