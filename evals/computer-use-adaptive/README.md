# Computer Use adaptive evaluation harness

Provider-neutral, deterministic evaluation for GitHub issues
[#435](https://github.com/chriscase/GrokPtah/issues/435),
[#272](https://github.com/chriscase/GrokPtah/issues/272),
[#274](https://github.com/chriscase/GrokPtah/issues/274), and
[#363](https://github.com/chriscase/GrokPtah/issues/363).

This crate owns **fixtures, metrics, and reports only**. It does not edit
production adaptive-profile runtime, provider-send ledgers, headless/broker
adapters, native helpers, or VM backends.

## Source gate

Exact tree: `origin/main` at `67e29bd34dc64049432c715c93c2cef2185c63ea`.
Unmerged adaptive runtime on developer checkouts is **not** authoritative.

## Commands

Zero provider calls. Do not run a workspace-wide build.

```sh
cd evals/computer-use-adaptive
cargo test --locked -- --test-threads=1
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --bin grokptah-cu-adaptive-eval -- --out campaign-out --repeats 5 --seed 435272
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --verify-report campaign-out/campaign-report.json \
  --verify-evidence campaign-out/campaign-evidence.json
```

`--repeats 0` and any result other than synthetic PASS exit nonzero. The verifier reconstructs the matrix and does not trust report totals.

Optional live continuation (same schemas, still does not call a provider in this
lane; the binary refuses unless you intended a later live adapter):

```sh
GROKPTAH_CU_ADAPTIVE_LIVE=1 cargo run --locked --bin grokptah-cu-adaptive-eval -- --out campaign-out
# exits 2: live is not implemented here on purpose
```

## Profiles

Canonical #435 names: `economy`, `balanced`, `high_assurance`.

Compatibility aliases (ingest only, never product rename):
`efficient` → `economy`, `frontier` → `high_assurance`.

Safety policy is identical across profiles. Economy means less
observation/action/model cost, never weaker checks.

## What this measures

Task success denominator, unauthorized dispatches, invalid actions, stale-action
attempts, abstentions, escalations, postcondition failures, recovery after two
restarts, observation/image bytes, model input/output **eval units** (compact
observation bytes, not vendor tokens), virtual latency, and cost **only when
authoritatively known**. Fake adapters always leave `costUsd` null. A safety
violation is always `FAIL_CLOSED`.
