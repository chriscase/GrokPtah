# Always-on Grokbot process certification

This campaign proves a durable always-on Grokbot on a **real standalone `grokptah-service` process**, driven only through authenticated MCP, with a **loopback fake provider that can hold a POST after acceptance**. It does not call live xAI. `GROKPTAH_AGENT_OFFLINE` is not a substitute for the scripted provider.

Base SHA: `67e29bd34dc64049432c715c93c2cef2185c63ea`.

Fixture: `crates/codegen/grokptah-service/tests/fixtures/always_on_grokbot.json` (`grokptah.always_on_grokbot_fixture.v1`, schemaVersion 1). Tests load and version-check it.

## Proven

1. Opt-in autonomous Manager plan (`step-a` then `step-b`) advances by polling public authenticated MCP only. No `ptah_tick_manager_plan` is used to drive the DAG. A duplicate tick after `succeeded` is an idempotence negative control.
2. Exact happy-path oracle keyed to the campaign plan, not setup Runs: one `manager-decision` Work, one observed `manager_proposal` Run, exactly one native Work/intent/attempt/Run and one provider POST per `step-a` / `step-b` / `step-b-fix`, all terminal runs `usagePendingRequests=0`, plan `succeeded`.
3. Same `request_id` + same payload replays the same plan id with no semantic growth. Same id + different payload returns `conflict`.
4. Fresh-home, table-driven restart scenarios (`step-a` materialized / succeeded, `step-b` failed, manager-decision succeeded, `step-b-fix` materialized, plan succeeded) assert the exact target Work/intent/attempt/Run identities. They do not accept setup Runs, any inbox message, or “plan already succeeded” as a stand-in for an earlier cut.
5. In-flight barrier: wait until the exact `step-a` provider POST is accepted and held, SIGKILL the service, reopen the same `GROKPTAH_HOME`. Semantic POST count stays 1. The original Run is `interrupted` with pending usage 0. Work is `failed` (not queued). No second Run/attempt/intent appears without an explicit user action. Attempts are public via `ptah_get_work.attempts` and `intent.attemptId`.
6. Fail-closed: invalid manager directive produces no replacement step/work; cancel + restart stays `cancelled` with no resend; malformed / disconnect / slow provider results are bounded terminal; missing/wrong MCP bearer and outside/escaping-symlink workspaces reject with no durable mutation. In-root canonicalizing symlinks are accepted by production allowlist code and are **not** claimed rejected.
7. Redaction: every MCP structured+text result, bounded stderr, soak report, and persisted-home text is scanned for bearer/API-key/live-base-URL sentinels. The fake provider records method/path/auth-presence/body digest/semantic id and never stores raw secrets.
8. Certification-lab probe `always-on-grokbot-lifecycle-v1` stamps only observed actions/oracles and only transitions it actually saw. Manifest and report mutation tests fail if a required action, oracle, or transition is dropped.

## Explicitly unverified / out of scope

- **Proposal-only enforcement** is `unverified-pending-pr-352`. Observing `purpose=manager_proposal` is not enforcement. A malicious tool-call side-effect proof is not claimed here.
- **Internal persistence cuts** (`occurrence-reserved`, `native-intent-persisted`, and the rest of the original nine names) are `not-proven-no-production-failpoint`. This campaign uses externally observable process boundaries only.
- **FakeClock / deterministic scheduler**: no public clock seam exists on the standalone process. Polls are **bounded/race-controlled**, not deterministic.
- **24h soak**: the ignored `soak_always_on_grokbot_24h` command is retained. It has **not been run**. Do not treat a 1-cycle CI smoke or a 10-minute ignored soak as a 24h pass.
- **Live xAI**: out of scope. Ambient `XAI_*` / `GROKPTAH_TOKEN_COMMAND` values are stripped before injecting loopback test credentials.
- **Desktop advisory-lock flake**: `.github/workflows/desktop.yml` already notes store tests use advisory file locks and `--test-threads=1`. That flake is **out of scope** for this child PR and is not fixed here.

## Short campaign (CI)

Ubuntu CI installs `libdbus-1-dev` and `pkg-config` before Cargo, then fmt, clippy `-D warnings`, process integration, lab units, and the actual probe.

```bash
sudo apt-get update && sudo apt-get install -y libdbus-1-dev pkg-config
cargo fmt --check --manifest-path crates/codegen/grokptah-service/Cargo.toml
cargo clippy --locked --all-targets --manifest-path crates/codegen/grokptah-service/Cargo.toml -- -D warnings
cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml --test always_on_grokbot -- --test-threads=1
cargo fmt --check --manifest-path evals/certification-lab/Cargo.toml
cargo clippy --locked --all-targets --manifest-path evals/certification-lab/Cargo.toml -- -D warnings
cargo test --locked --manifest-path evals/certification-lab/Cargo.toml -- --test-threads=1
cargo build --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml --bin grokptah-service
export GROKPTAH_SERVICE_BIN="$PWD/crates/codegen/grokptah-service/target/debug/grokptah-service"
# Unset ambient XAI_API_KEY / XAI_API_BASE / GROKPTAH_TOKEN_COMMAND.
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --probe always-on-grokbot-lifecycle-v1
```

Generated reports stay under the output path and are not committed.

## Restart scenarios (externally observable)

Each row uses a **fresh home and fresh plan**. Predicates key to the named target Work/intent/Run/provider request.

1. `step-a-work-materialized`
2. `step-a-in-flight-provider-hold` (held POST, SIGKILL, no hidden resend)
3. `step-a-succeeded`
4. `step-b-failed`
5. `manager-decision-succeeded`
6. `step-b-fix-materialized`
7. `plan-succeeded`

A bounded `/ready` poll is used after spawn. That is readiness, not a test `sleep`. Native executor and manager supervisor intervals are production (1s / 2s).

## Soak

CI runs **one** bounded barrier-restart cycle and writes a machine-readable report (commit SHA, fixture schema/hash, duration, cycles, restarts, sends, max RSS/FD/thread/disk/latency, redaction, SHA-256). That is not a 10-minute or 24-hour pass.

```bash
# 10 minutes (development, ignored)
GROKBOT_SOAK_SECS=600 cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot soak_always_on_grokbot_10m -- --ignored --nocapture

# 24 hours (release retained command; not run in this PR)
GROKBOT_SOAK_SECS=86400 cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot soak_always_on_grokbot_24h -- --ignored --nocapture
```
