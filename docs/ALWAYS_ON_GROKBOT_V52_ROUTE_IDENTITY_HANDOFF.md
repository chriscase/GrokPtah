# Always-On Grokbot v52 route-identity repair handoff

Status: **external source/service qualification only; no Always-On
certification is claimed by this document.**

This is the exact follow-up to the v51 fail-closed campaign. v51 reached the
real `grokptah-service` process, but `ptah_get_run` returned MCP `internal`
because the public projection treated the allowlisted `grok-build` route
identity as a privileged needle. This candidate exempts only the typed,
exactly allowlisted public route summary and keeps all free-form fields
subject to the existing redaction scan.

## Frozen candidate

- Immutable bundle: `/private/tmp/grokptah-dream-stage4-v52-public-run-correction.bundle`
- Bundle SHA-256: `56dad64886b77195ad5dac3fe48d4c9cec12dd7c96014bc7d23ee21888f44a0b`
- Candidate branch in the bundle: `codex/v52-public-run-route-identity-correction`
- Candidate head: `6e9ee187a846f26c3210ac2e417ed16115813cad`
- Parent (v51 failure head): `bb9e7ed30f19018d6c3244885a6ce83818662c1b`
- Parent of v51 failure head: `a13a0482e12650f06120ef7f4317b977b25f6f5e`
- Changed file: `crates/codegen/grokptah-agent-bridge/src/orchestration/public_run.rs`
- Developer checkout: `6409645cb7d0fe6d75585f0610366340f808b8ec` (must remain untouched)

The bundle is complete and contains only the one-file repair commit on top of
the v51 failure head. Do not merge, push, rebase, undraft, create a PR, or
patch the disposable checkout during qualification.

## Required external procedure

Create a disposable checkout from the bundle and explicitly select
`6e9ee187a846f26c3210ac2e417ed16115813cad`. Before every Rust command, report
disk headroom and active cargo/rustc ownership, then set exactly:

```sh
export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
```

Reuse the target serially. Never create an in-checkout or per-agent target,
kill another owner, or kill the shared sccache daemon. Report target size,
open handles, and cleanup/retention after the lane.

Run these checks in order:

```sh
git bundle verify /private/tmp/grokptah-dream-stage4-v52-public-run-correction.bundle
test "$(git rev-parse --verify '6e9ee187a846f26c3210ac2e417ed16115813cad^{commit}')" = \
  6e9ee187a846f26c3210ac2e417ed16115813cad
rustfmt --edition 2021 --check \
  crates/codegen/grokptah-agent-bridge/src/orchestration/public_run.rs
cargo metadata --locked --offline --no-deps --format-version=1 >/dev/null
cargo test --locked \
  --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib bare_xai_selection_key_may_equal_the_public_model_identity \
  -- --test-threads=1
cargo test --locked \
  --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot -- --test-threads=1
```

The focused regression must pass, and every `ptah_get_run` call in the real
service-process campaign must return a redacted `PublicRun`; an MCP `internal`
response is a failure. If either phase fails, return `NOT QUALIFIED` with the
first exact failure and stop. Do not patch the candidate in the campaign
checkout.

Only after both phases pass, run the documented full service suite, certification
lab probe, and ignored soak from
[`ALWAYS_ON_GROKBOT_V52_HANDOFF.md`](ALWAYS_ON_GROKBOT_V52_HANDOFF.md), using
this exact candidate and the same cache policy. The short campaign, lab probe,
and retained soak must all agree on the candidate before any Stage 3–6 claim.

## Required report

Return one dated, secret-free report containing the exact candidate and binary
SHAs, bundle verification, pre/post disk and target ownership, focused test,
real-process short campaign, full-suite, lab-probe, and soak results, public
route-redaction evidence, restart/no-duplicate evidence, and an explicit
`QUALIFIED` or `NOT QUALIFIED` decision. A source pass or focused unit pass is
not Always-On certification and does not close #301 or #305.
