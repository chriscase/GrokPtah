# Stage 6 evidence-hardening external handoff

This is a copyable Grok Build procedure for the separate Stage 6 source
candidate. It is intentionally fail-closed: a passing short smoke is not a
Stage 6 certification, and the 72-hour result is not eligible unless the
earlier Stage 3–5 gates are already independently complete.

## Immutable input

- Bundle: `/private/tmp/grokptah-stage6-evidence-hardening-v1-exact-984ff9a.bundle`
- SHA-256: `cfa741b67c51bc9804b566440a855348727d398c7d97279b2e61e5cbeb12b91b`
- Required head: `984ff9a4b13a6f2eb2054c84d5880abd5a0d4e1a`
- Required parent: `5406bbea059371392b0d77d58cca083640244a6c`
- Source worktree: `/private/tmp/grokptah-stage6-evidence-hardening`

The original external bundle is stale and must not be used:
`/private/tmp/grokptah-stage6-evidence-hardening-v1.bundle`.

Before sending the Grok Build prompt, run the repository-owned guard:

```bash
sh docs/verify-stage6-evidence-hardening.sh
```

It verifies the bundle digest, complete history, exact advertised head, exact
source checkout head, clean worktree, and required parent relationship.

## Grok Build prompt

```text
Run the Stage 6 evidence-hardening qualification from the immutable bundle
below. Do not change the developer checkout, push, merge, rebase, undraft, or
open a PR. Use a disposable checkout and stop immediately on any identity,
clean-worktree, prerequisite, secret-scan, or resource-policy failure.

INPUT_BUNDLE=/private/tmp/grokptah-stage6-evidence-hardening-v1-exact-984ff9a.bundle
INPUT_SHA256=cfa741b67c51bc9804b566440a855348727d398c7d97279b2e61e5cbeb12b91b
EXPECTED_HEAD=984ff9a4b13a6f2eb2054c84d5880abd5a0d4e1a
EXPECTED_PARENT=5406bbea059371392b0d77d58cca083640244a6c

Before any build, print df, cargo/rustc ownership, and the target path. Verify
the bundle SHA-256, `git bundle verify`, exact HEAD/parent, and a clean
worktree. Every Cargo command must set these exact variables; reuse the target
serially and never create an in-checkout or per-agent target:

RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default

Run first (bounded compile/runtime gate):
cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot stage6_multi_worker_restart_rotation_smoke \
  -- --test-threads=1 --nocapture

Only if the exact earlier Stage 3–5 campaign artifacts are present and
independently verified, run the release campaign below. It must be the clean
unchanged candidate HEAD and must retain the printed secret-free report path:

GROKBOT_SOAK_SECS=259200 cargo test --locked \
  --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot certify_stage6_multi_worker_72h \
  -- --ignored --test-threads=1 --nocapture

The release gate requires exactly 259200 seconds, at least three restarts,
two independent Agent-bound worker credentials with rotation, zero duplicate
attempts, fail-closed stale claims, bounded parent/child cycles, complete
secret-free evidence validation, and no credential/path/base-URL sentinel in
stdout, stderr, the persisted home, or the retained report. Do not infer a
pass from source inspection or a short smoke. Report the exact head, bundle
digest, commands, timings, resource maxima, retained artifact path, cleanup
status, and every unmet prerequisite. If any required condition is absent,
report NOT_QUALIFIED and do not emit a certification claim.
```

## Current status

The input bundle is now exact and complete, but no external smoke or 72-hour
campaign result has been returned. Stage 6 remains open.
