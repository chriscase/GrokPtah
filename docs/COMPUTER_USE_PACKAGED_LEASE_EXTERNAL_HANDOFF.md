# Packaged isolated-guest lease-fence external handoff

Status: **qualification procedure only; no packaged VM or hardware claim is
made by this document.**

The candidate follow-up at `b250b70` threads the exact one-Agent lease into
the macOS packaged-runtime supervisor. The supervisor must now reject start,
frame reads, guest input, and stop without a matching agent/revision lease;
failure revokes ownership; cleanup is refused while ownership remains.

## Frozen candidate

- Candidate branch: `codex/cu-isolated-guest-bootstrap-v1`
- Candidate head: `b250b70`
- PR #374 source parent: `295a4ff62939af1a3034119653c83c7a0a2e1bff`
- PR #374 head: `5919e3343af20a78e17459b8ac8454bbc5aeca7e`
- Main/base: `67e29bd34dc64049432c715c93c2cef2185c63ea`
- Developer checkout: `6409645cb7d0fe6d75585f0610366340f808b8ec` (must remain untouched)

Use a disposable checkout at this exact head. Do not merge, push, rebase,
undraft, create a PR, modify the developer checkout, or alter any existing
app/session/worktree.

## External build contract

Before every Rust command, report disk headroom and active cargo/rustc owners.
Use exactly this serial cache policy:

```sh
export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
```

Reuse that target serially. Never create an in-checkout or per-agent target;
do not kill another owner’s process or sccache daemon. Report target size,
process ownership, open-handle state, and cleanup/retention after the lane.

Run, in order:

```sh
rustfmt --edition 2021 --check \
  crates/codegen/grokptah-agent-bridge/src/computer_use/isolated_guest.rs \
  crates/codegen/grokptah-agent-bridge/src/computer_use/macos_isolated_runtime.rs

cargo metadata --locked --offline --no-deps --format-version=1 >/dev/null

cargo test --locked \
  --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib isolated_guest -- --test-threads=1

cargo test --locked \
  --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib native_launch_descriptor_set_must_be_complete_and_unique -- --test-threads=1
```

The focused tests must compile the changed macOS supervisor signatures, not
just the simulator module. If any phase fails, return `NOT QUALIFIED` with the
first exact failure and stop; do not patch the disposable checkout during the
verification lane.

Before the external build, the local source-boundary check may be run without
Rust compilation:

```sh
bash docs/verify-packaged-lease-fence.sh
```

It only proves that the intended lease markers and candidate identity are
present. It is not a substitute for the external compile/test commands below.

## Interpretation

Return `PASS` only for the source lease-fence checks above, with exact SHA and
secret-free output. This does **not** qualify a signed helper, guest image,
Virtualization.framework launch, real guest boot, rendered frames, host input,
cleanup on a live VM, or a long soak. Those remain mandatory Stage 9/#288
hardware and packaged-runtime gates and require a separate credentialed host
campaign.
