# Always-On Grokbot v52 external certification handoff

Status: **verification procedure only; no Always-On certification is claimed here.**

This handoff addresses the v51 fail-closed result. v51 reached `ptah_submit_task`, but every
`ptah_get_run` response failed because a route-secret needle remained in an allowlisted public
Run field. The candidate below contains the complete public-projection scrub and regression test;
the external service campaign must prove that the real `grokptah-service` process now returns a
redacted public Run instead of an MCP internal error.

## Exact candidate

- Source bundle: `/private/tmp/grokptah-cu-isolated-visual-v12.bundle`
- Bundle SHA-256: `5e96b4021857c37a0b07d7ab174f2cd6927f0919a458cd03dfc2cb4c04d1bc5a`
- Source cutoff: `baa28f748c13a6ceda381e068004cd46aea2658c`
- Branch in the bundle: `codex/cu-isolated-guest-bootstrap-v1`
- Base/main reference: `67e29bd34dc64049432c715c93c2cef2185c63ea`

The bundle is immutable. Do not use the v51 input bundle or its lock-only child as the v52
candidate; those revisions predate the public-projection repair.

## Paste this to the external build owner

```text
Run the Always-On Grokbot v52 external certification campaign for the exact candidate below.
This is a fail-closed verification campaign, not an implementation task.

Bundle: /private/tmp/grokptah-cu-isolated-visual-v12.bundle
Bundle SHA-256: 5e96b4021857c37a0b07d7ab174f2cd6927f0919a458cd03dfc2cb4c04d1bc5a
Source cutoff: baa28f748c13a6ceda381e068004cd46aea2658c
Base/main: 67e29bd34dc64049432c715c93c2cef2185c63ea

Create a disposable checkout from that bundle. Do not modify Chris’s checkout, any existing
worktree, branch, PR, GitHub state, or app session. Do not merge, push, rebase, or create a PR.
Keep the candidate source clean and record the exact checkout SHA before building.

Before any Rust command, report disk headroom and active cargo/rustc ownership, then set exactly:

RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default

Reuse that target serially. Never create an in-checkout or per-agent target. Report target size,
process/lsof ownership, and cleanup or retention status after the campaign. Do not kill a shared
sccache daemon or another owner’s build.

Run the repository’s static checks first. Then run this targeted regression against the real
service crate:

cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  public_projection_redacts_route_needles_in_every_allowlisted_string -- --test-threads=1

The test must pass. Next run the short real-process Always-On campaign, overwriting its two
secret-free evidence files in one command:

cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot -- --test-threads=1

Required result: every ptah_get_run call succeeds with a redacted PublicRun; no MCP internal
error may be hidden by a narrow unit test. If it fails, stop and retain a bounded failure report
with the first exact failing phase; do not patch the candidate in the campaign checkout.

If the short campaign passes, run the full service suite excluding only the documented hosted
desktop parity case:

cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml -- \
  --test-threads=1 --skip shared_black_box_v1_desktop_hosted_parity

Build the exact service binary for the lab probe using the same explicit cache variables, set
GROKPTAH_SERVICE_BIN to that binary, and run:

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- run \
  --repository "$PWD" --probe always-on-grokbot-lifecycle-v1

The lab probe must produce a secret-free report whose certification_ready decision is independently
consistent with the exact candidate SHA. Then run the ignored soak only if every earlier phase is
green:

GROKBOT_SOAK_SECS=600 cargo test --locked \
  --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  soak_always_on_grokbot -- --ignored --nocapture --test-threads=1

Use 86400 seconds only when disk, process ownership, and overnight retention are explicitly safe.
The soak is not a substitute for the real-process short campaign or the lab probe.

Return one dated, secret-free report containing: exact candidate/binary SHAs, pre/post disk and
target ownership, short-campaign result, full-suite result, lab-probe result, soak duration,
restart/duplicate-send evidence, route-redaction evidence, credential/quota handling, and an
explicit QUALIFIED or NOT QUALIFIED decision. Do not claim certification if any phase is skipped,
if ptah_get_run returns MCP internal, if evidence files are stale/overwritten incompletely, or if
the exact candidate/binary identity is missing. Do not merge or undraft anything.
```

## Required interpretation

The v51 report is a valid **NOT QUALIFIED** result, not a partial certification. A v52 result is
eligible for consideration only when the real service process, lab probe, and retained soak all
agree on the exact candidate and the evidence verifier accepts the report. This handoff does not
close the Always-On/#301/#305 gates by itself.
