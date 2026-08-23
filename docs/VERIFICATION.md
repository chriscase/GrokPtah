# Verification paths

This repository vendors xAI's `grok-build` crate tree (`crates/codegen/xai-*`,
`crates/common/*`) alongside the fork's own code (`desktop/`,
`crates/codegen/grokptah-agent-bridge`). The upstream crates are developed and
tested in xAI's **Bazel** monorepo; this fork verifies the code it owns with
`cargo` + `npm`. Those two facts determine which commands are supported.

## Supported paths (what CI runs, and what you should run)

All are runnable from a clean clone with caches disabled
(`CARGO_INCREMENTAL=0`, `RUSTC_WRAPPER` unset). See `.github/workflows/desktop.yml`. Hosted-service CI is `.github/workflows/hosted-service.yml`
and runs when the service crate, the shared bridge contract, lockfiles, or that workflow change.

| Area | Command | Working dir |
|------|---------|-------------|
| Frontend | `npm ci && npm run typecheck && npm test` | `desktop` |
| Desktop shell | `cargo test --locked` | `desktop/src-tauri` |
| Agent bridge | `cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -- --test-threads=1` | `crates/codegen/grokptah-agent-bridge` |
| Hosted service | `cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -- --test-threads=1` | `crates/codegen/grokptah-service` |
| Offline oracles | `cargo test --locked eval_oracle -- --nocapture` | `crates/codegen/grokptah-agent-bridge` |
| Focused upstream support | `cargo fmt -p xai-grok-env -p xai-grok-shell-base -- --check && cargo clippy -p xai-grok-env -p xai-grok-shell-base --all-targets --all-features --locked -- -D warnings && cargo test -p xai-grok-shell-base --all-features --locked` | repository root |

The deterministic reliability campaign is also a supported focused check:

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --locked --test reliability_eval -- --test-threads=1
```

For the report-producing operator command and scenario matrix, see
[RELIABILITY_EVALS.md](RELIABILITY_EVALS.md).

## Unsigned release build

The manually dispatched `Desktop Release Build` workflow builds the reviewed
commit on a GitHub-hosted macOS runner and uploads an unsigned app/DMG bundle
for seven days. It does not sign, notarize, publish, or use release secrets.
Regular Desktop CI and this workflow also syntax-check and link the
isolated-visual helper candidate, but deliberately do not embed that unsigned
helper or a guest image in the uploaded app. That source check is not packaged
identity or VM evidence.

The workflow keeps `CARGO_HOME` and `CARGO_TARGET_DIR` under the runner's
private temporary directory. Its cache key includes the OS, architecture,
Rust toolchain, frontend lockfile, both desktop/bridge lockfiles, and bundle
configuration. The cache is an accelerator only: a miss uses the ordinary
`npm ci` + `tauri build` path, and restored Cargo artifacts remain subject to
Cargo fingerprint validation. No pull request trigger or self-hosted runner is
used.

Local DMG creation can still fail because `hdiutil` and Finder state are
environment-sensitive. In that case, `npm run tauri:build -- --bundles app`
still verifies the compiled unsigned application without changing signing or
notarization policy.


Other focused upstream crates may be tested individually, but their Cargo
feature wiring varies because Bazel remains the parent project's primary gate.

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
clobbered on the next vendor sync. Verify relevant upstream crates individually,
or with Bazel, and keep any intentional backport aligned with the parent.

### One completed exception

`xai-grok-env` exposes its `EnvVarGuard` test helper behind a `test-support`
feature (in addition to `#[cfg(test)]`), and `xai-grok-shell-base` forwards that
feature for its own downstream test seams. This is a focused backport of the
parent project's wiring in Grok Build commit `8adf901`; it makes
`cargo test -p xai-grok-shell-base` pass standalone while keeping the fork
aligned with the parent.
