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

Exact tree: `origin/main` at `c6f1cb23e9d6217005599850d9e0d6f7df64d5a1`.
Unmerged adaptive runtime on developer checkouts is **not** authoritative.

## Commands

Zero provider calls. Do not run a workspace-wide build.

```sh
REPO="$(git rev-parse --show-toplevel)"
EXPECTED_HEAD="$(git -C "$REPO" rev-parse HEAD)"
EXPECTED_BASE=c6f1cb23e9d6217005599850d9e0d6f7df64d5a1
cd evals/computer-use-adaptive
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --out campaign-out --repeats 5 --seed 435272 \
  --repository "$REPO" --expected-head "$EXPECTED_HEAD" --source-gate "$EXPECTED_BASE"
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --verify-report campaign-out/campaign-report.json \
  --verify-evidence campaign-out/campaign-evidence.json \
  --repository "$REPO" --expected-head "$EXPECTED_HEAD" --source-gate "$EXPECTED_BASE"
```

`--repository`, `--expected-head`, and `--source-gate` are mandatory for both
generation and verification. Dirty worktrees, mismatched heads/trees/bases, and
any result other than synthetic PASS fail closed. The verifier reconstructs the
matrix and dispatch authority; it does not trust report totals or a physical
record's own `permitted` flag.

Library callers must supply the matching `EvidenceSet` to `verify_campaign` or
use `verify_json_with_evidence`. The report-only `verify_report` and
`verify_json` entry points are diagnostic parsers and deliberately return
`VERIFIER_ERROR` even for an otherwise coherent report; a report without its
typed evidence can never produce release PASS.

The public `run_campaign` API likewise requires an explicit repository,
expected candidate head, and expected base. It refuses a dirty tree or a head
or ancestry mismatch. The lower-level pre-observed-source constructor is
crate-private so callers cannot mint provenance and then ask the library to
bless it.

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
