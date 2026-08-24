# Packaged lease stop-failure follow-up handoff

Status: **future external source qualification only**. This follow-up is
separate from the active `b250b70` campaign and makes no packaged VM or
hardware claim.

## Frozen candidate

- Candidate source commit: `f561dd8`
- Candidate branch: `codex/cu-isolated-guest-bootstrap-v1`
- Immutable bundle: `/private/tmp/grokptah-packaged-lease-stop-failure-f561dd8-v1.bundle`
- Bundle SHA-256: `63d976bf2e1c45fe92cb9c2540f702008143e3b7d88bd7f208ed3adf79c583a7`
- Parent: `b467b28`
- Base/main: `67e29bd34dc64049432c715c93c2cef2185c63ea`

The correction revokes the packaged guest lease after every terminal stop
result, including bounded helper-reap failure. Cleanup still requires exact
process, handle, overlay, and frame-cache evidence; revoking ownership does
not make uncertain cleanup successful. A stale or failed stop cannot resume
input.

Create a disposable checkout from the bundle and explicitly detach at
`f561dd8`. Do not merge, push, rebase, undraft, create a PR, modify the
developer checkout, or alter any existing app/session/worktree. The bundle's
default branch may be later than the frozen source commit; the explicit
detached checkout is mandatory.

Before every Rust command, report disk headroom and active cargo/rustc owners,
then set exactly:

```sh
export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
```

Reuse that target serially. Never create an in-checkout or per-agent target;
report target ownership, open handles, and cleanup/retention afterward.

Run, in order:

```sh
bash docs/verify-packaged-lease-fence.sh
rustfmt --edition 2021 --check \
  crates/codegen/grokptah-agent-bridge/src/computer_use/macos_isolated_runtime.rs
cargo metadata --locked --offline --no-deps --format-version=1 >/dev/null
cargo test --locked \
  --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib terminal_stop_revokes_lease_even_when_reaping_fails -- --test-threads=1
```

The focused test must compile the changed macOS supervisor source. Return
`PASS` only with the exact source SHA, the stop-failure regression green, and
secret-free evidence. This still does not qualify a signed helper, guest
image, VM launch, real boot/render/input, cleanup on a live VM, or soak.
