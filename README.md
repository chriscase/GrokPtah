# GrokPtah

**GrokPtah** is a desktop-first AI coding agent: open a project, chat with the agent, approve tools, review diffs, and run an integrated terminal — all in a native window.

The name combines **Grok** (the agent lineage) with **Ptah** (Egyptian craftsman-creator), reflecting a tool meant for *building* software, not only chatting about it.

| | |
|--|--|
| **Product** | GrokPtah desktop (Tauri 2 + React) |
| **Origin** | Fork of [xai-org/grok-build](https://github.com/xai-org/grok-build) (Grok Build / SpaceXAI) |
| **License** | [Apache License 2.0](LICENSE) |
| **Repo** | [chriscase/GrokPtah](https://github.com/chriscase/GrokPtah) |
| **Architecture** | [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) |
| **Dev setup** | [`docs/DEV_SETUP.md`](docs/DEV_SETUP.md) |

---

## What is this?

GrokPtah is a **local coding agent harness** with two clients over the same kind of workflow:

1. **Desktop app (primary for this fork)** — a fullscreen Tauri application with a React UI: sessions, streaming replies, tool cards, permissions, plan mode, file tree, git, MCP/plugins/skills, settings, subagents, background tasks, and multi-tab terminal.
2. **CLI / TUI (upstream path)** — the original terminal UI from Grok Build (`xai-grok-pager`), still buildable from this tree for people who prefer the ratatui pager.

The agent can read and edit files, search the codebase, run shell commands (with approval), and — when you provide an xAI API key — call live models for the reply. Shell tool runs stream into the UI as a **single live process** (the terminal attaches to that stream; it does not re-run the command).

---

## Why this fork exists

Upstream **Grok Build** is a strong terminal agent (ACP, tools, MCP, sessions). GrokPtah’s purpose is to take that lineage and ship a **native desktop product**:

| Goal | Approach |
|------|----------|
| Desktop UX | Tauri 2 window + React/Vite UI (not a terminal skin) |
| Simple process model | **In-process** agent host (`grokptah-agent-bridge`) — no second `grok agent stdio` child on the happy path |
| Keep the TUI | Upstream CLI packages remain independently buildable |
| Own identity | Product name **GrokPtah**, own icons/about, **no** upstream xAI CLI auto-update prompts on desktop |

This is **not** an official xAI product. It is a personal/community fork for desktop development under Apache-2.0.

---

## Features (desktop)

- **Chat** — multi-line composer, streaming assistant + thought chunks (Gemini-style fade-in), stop/cancel turn  
- **Tools** — cards for read/search/edit/shell; permission modal (allow / deny / always)  
- **Plan mode** — propose steps; accept or reject from the UI  
- **Sessions** — separate **Builds** (coding agent) and **Chats** (plain Grok); multi-tab; full-screen browser (rename / delete / archive / folders / tags); durable under `~/.grokptah/`  
- **Search** — hybrid **keyword + semantic (TF–IDF)** search across chats and builds (filter by kind, archive, folder, tag)  


- **Project** — open folder (native dialog); rules discovery (`AGENTS.md`, etc.)  
- **Files & git** — tree, fuzzy open, status/diff/stage/commit, worktree list, agent-edit diffs  
- **Terminal** — interactive multi-tab PTY; tool shells attach to the **live** tool process stream  
- **Extensibility** — MCP config under `~/.grokptah` / project `.mcp.json`; plugins & skills on disk; hooks config  
- **Auth** — store an xAI API key in the OS keychain (or `XAI_API_KEY`); open console for keys  
- **Settings** — model, effort, sandbox profile, appearance, permission mode  

Capability parity with the TUI is the target; the UI is desktop-native, not a pixel clone of the terminal theme.

---

## Quick start

### Prerequisites (macOS)

- **Rust** — version pinned in [`rust-toolchain.toml`](rust-toolchain.toml) (`rustup` installs it)  
- **protoc** — e.g. `brew install protobuf`  
- **Node.js 20+** — for the desktop UI  
- **Xcode Command Line Tools** — for Tauri on macOS  

Details: [`docs/DEV_SETUP.md`](docs/DEV_SETUP.md).

### Desktop (recommended)

```sh
git clone https://github.com/chriscase/GrokPtah.git
cd GrokPtah/desktop
npm install
npm run tauri:dev
```

Package a macOS app (unsigned is fine for local use):

```sh
cd desktop
npm run tauri:build
# → desktop/src-tauri/target/release/bundle/macos/GrokPtah.app
# → desktop/src-tauri/target/release/bundle/dmg/GrokPtah_*.dmg
```

On first use: **Open folder**, optionally **Save key** (xAI API key) or set `XAI_API_KEY`.

### CLI / TUI (upstream-style)

```sh
# from repo root
cargo run -p xai-grok-pager-bin
# release binary: target/release/xai-grok-pager
# (official Grok Build installs ship this class of binary as `grok`)
```

```sh
cargo check -p xai-grok-pager-bin
```

---

## How it fits together

```
┌─────────────────────────────────────────────────────────┐
│  GrokPtah desktop (desktop/)                            │
│  React + Vite in Tauri 2 webview                        │
└──────────────────────────┬──────────────────────────────┘
                           │ commands + events
┌──────────────────────────▼──────────────────────────────┐
│  grokptah-desktop (src-tauri)                           │
│  dialogs · PTY hub · thin IPC                           │
└──────────────────────────┬──────────────────────────────┘
                           │ in-process
┌──────────────────────────▼──────────────────────────────┐
│  grokptah-agent-bridge                                  │
│  sessions · stream · permissions · tools · shell kill   │
└─────────────────────────────────────────────────────────┘

Separately (same monorepo root workspace):

  cargo run -p xai-grok-pager-bin   → classic TUI client
```

Root [`Cargo.toml`](Cargo.toml) is largely **upstream-generated** — treat it as read-only. Desktop uses a **nested** Cargo workspace under `desktop/src-tauri` so GrokPtah can evolve without rewriting the monorepo root.

Full design notes: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## Repository layout

| Path | Role |
|------|------|
| `desktop/` | GrokPtah app — frontend + Tauri backend |
| `crates/codegen/grokptah-agent-bridge/` | In-process agent host (desktop happy path) |
| `crates/codegen/xai-grok-pager*` | Upstream TUI / pager |
| `crates/codegen/xai-grok-shell` | Upstream agent runtime (CLI/ACP) |
| `docs/` | Architecture and developer setup |
| `~/.grokptah/` | Local MCP, plugins, skills, hooks (created on use) |

---

## Development

```sh
# Desktop UI
cd desktop
npm run typecheck
npm test
npm run tauri:dev

# Agent bridge (from its crate directory)
cd crates/codegen/grokptah-agent-bridge
cargo test
cargo check

# CLI package (repo root)
cargo check -p xai-grok-pager-bin
```

Prefer **per-crate** `cargo check` / `cargo test` targets; full-workspace builds of the upstream tree are large and slow.

CI for desktop paths: [`.github/workflows/desktop.yml`](.github/workflows/desktop.yml).

---

## Configuration & data

| Item | Where |
|------|--------|
| API key | OS keychain (desktop “Save key”) or env `XAI_API_KEY` |
| Optional API base | `XAI_API_BASE` (default `https://api.x.ai/v1`) |
| MCP servers | `~/.grokptah/mcp.json`, project `.mcp.json` |
| Plugins / skills | `~/.grokptah/plugins`, `~/.grokptah/skills` |
| Hooks | `~/.grokptah/hooks.json` (and project overrides when present) |

Desktop builds **do not** run the upstream xAI CLI auto-updater, so the app will not prompt you to replace GrokPtah with the official `grok` binary.

---

## Status & roadmap

This fork tracks a desktop **TUI feature-parity** milestone (chat, tools, sessions, files/git, terminal, MCP/plugins/skills, settings, packaging). See open/closed work on the repo’s [issues](https://github.com/chriscase/GrokPtah/issues) and [milestone](https://github.com/chriscase/GrokPtah/milestone/1).

Known intentional limits (today):

- Happy-path agent is the **bridge host** (local tools + optional live chat API), not a full embed of every upstream `xai-grok-shell` path.  
- Auth is API-key oriented (keychain + console), not a full interactive OAuth product flow.  
- Plugin “marketplace” is a **local catalog** on disk, not a remote store.  
- Primary packaging target is **macOS**.

---

## Contributing & security

- GrokPtah is developed in this fork for desktop work.  
- Upstream Grok Build’s [`CONTRIBUTING.md`](CONTRIBUTING.md) states that **xAI does not accept external PRs** on the original tree.  
- Security reports: see [`SECURITY.md`](SECURITY.md). Prefer private disclosure for vulnerabilities.

---

## License & credits

- **First-party GrokPtah code** — Apache License 2.0 ([`LICENSE`](LICENSE)).  
- **Upstream Grok Build** — same license family; © SpaceXAI / xAI and contributors.  
- **Third-party** — [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES), [`third_party/NOTICE`](third_party/NOTICE), and crate-local notices under `crates/codegen/`.

Grok Build is the foundation. GrokPtah is the desktop craft layer on top.
