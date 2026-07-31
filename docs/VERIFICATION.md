# Verification paths

This repository vendors xAI's `grok-build` crate tree (`crates/codegen/xai-*`,
`crates/common/*`) alongside the fork's own code (`desktop/`,
`crates/codegen/grokptah-agent-bridge`). The upstream crates are developed and
tested in xAI's **Bazel** monorepo; this fork verifies the code it owns with
`cargo` + `npm`. Those two facts determine which commands are supported.

## Supported paths (what CI runs, and what you should run)

All are runnable from a clean clone with caches disabled
(`CARGO_INCREMENTAL=0`, `RUSTC_WRAPPER` unset). See `.github/workflows/desktop.yml`.

| Area | Command | Working dir |
|------|---------|-------------|
| Frontend | `npm ci && npm run typecheck && npm test` | `desktop` |
| Desktop shell | `cargo test --locked` | `desktop/src-tauri` |
| Agent bridge | `cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -- --test-threads=1` | `crates/codegen/grokptah-agent-bridge` |
| Offline oracles | `cargo test --locked eval_oracle -- --nocapture` | `crates/codegen/grokptah-agent-bridge` |

A focused single upstream crate can also be verified, e.g.
`cargo test -p xai-grok-shell-base`.

## Unsupported: root-wide `cargo test` / `cargo test --workspace [--no-run]`

**This is expected to fail and is not a defect.** The vendored upstream crates
share **test-only helpers across crate boundaries** — for example
`xai_grok_env::EnvVarGuard`, `ModifierDelivery::new_for_test`,
`PromptImagePreview::ready_for_test` — gated behind `#[cfg(test)]` and/or
per-crate `default-bazel` features. Bazel compiles that helper surface into the
shared rlib; a plain `cargo test --workspace` does not, so cross-crate test
builds fail with unresolved import / associated-item errors (`E0432`, `E0599`,
`E0433`, …). Reproducing on `main` shows this concentrated in the upstream TUI
crates (`xai-grok-pager` alone: ~160 such errors).

Do **not** try to make the whole workspace `cargo test` green by feature-gating
the upstream crates: it is a large, upstream-divergent change that would be
clobbered on the next vendor sync and adds no coverage the supported paths lack.
Verify upstream crates individually, or with Bazel.

### One completed exception

`xai-grok-env` exposes its `EnvVarGuard` test helper behind an off-by-default
`test-support` feature (in addition to `#[cfg(test)]`), and
`xai-grok-shell-base` enables it as a dev-dependency. This completes a fix that
was already scaffolded in `xai-grok-shell-base/Cargo.toml` and makes
`cargo test -p xai-grok-shell-base` pass standalone. It changes no existing
build config (the feature is additive and off by default).
