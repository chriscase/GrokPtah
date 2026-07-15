# GrokPtah development setup (macOS)

## Prerequisites

### Rust

Pinned by `rust-toolchain.toml` (currently **1.92.0**). `rustup` installs it
on first `cargo` invocation:

```sh
rustc --version   # should match rust-toolchain.toml
cargo --version
```

### protoc

Proto codegen needs `protoc` on `PATH` (or `$PROTOC`). Either:

```sh
brew install protobuf
protoc --version
```

or install [dotslash](https://dotslash-cli.com) so `bin/protoc` works.

### Node.js

Node **20+** and npm for the desktop UI:

```sh
node -v   # v20+
npm -v
```

### Tauri 2 system deps (macOS)

- Xcode Command Line Tools: `xcode-select --install`
- `cargo-tauri` CLI (optional; npm scripts use the local package):

```sh
cargo install tauri-cli --version "^2"
```

## CLI / TUI build

From the repo root:

```sh
cargo check -p xai-grok-pager-bin
cargo run -p xai-grok-pager-bin
```

Full workspace checks are slow; always target specific crates.

## Desktop build

```sh
cd desktop
npm install
npm run tauri dev      # dev window
npm run build          # Vite production bundle
npm run tauri build    # .app (unsigned OK)
npm run typecheck      # tsc --noEmit
npm test               # frontend unit tests
```

Bridge / Rust tests (from `desktop/src-tauri` nested workspace):

```sh
cd desktop/src-tauri
cargo test
cargo check
```

## Auth notes

Desktop can store tokens via the OS keychain when available. Live model calls
need network credentials; bridge tests and offline local tools do not.

## Scratch / evidence

Goal harness captures logs under the implementer scratch directory, not shared `/tmp`.
