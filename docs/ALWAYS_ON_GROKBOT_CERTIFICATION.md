# Always-on Grokbot certification

This campaign proves a durable always-on Grokbot on a **real standalone `grokptah-service` process**, driven only through authenticated MCP, with a **loopback fake provider**. It does not call live xAI. `GROKPTAH_AGENT_OFFLINE` is not a substitute for the scripted provider.

Base SHA: `67e29bd34dc64049432c715c93c2cef2185c63ea`.

## What it proves

1. Opt-in autonomous Manager plan with a dependent DAG (`step-a` then `step-b`).
2. Exactly one `manager-decision` Work item, admitted as proposal-only (`purpose=manager_proposal`).
3. Native admit/complete of `step-a`, controlled failure of `step-b`, one replacement directive (`step-b-fix`), plan `succeeded`.
4. Duplicate `ptah_tick_manager_plan` after every stage does not duplicate provider sends, Runs, Work, decisions, intents, or messages.
5. Process-level kill/restart on the **same** `GROKPTAH_HOME` at nine named cut points never silently resumes an uncertain provider attempt (`usagePendingRequests` on Interrupted runs is 0).
6. Fail-closed: autonomous-with-approval, stale plan revision, invalid directive, cancellation, and the documented **absence** of a quota ledger at this SHA (cardinality 0).

Quota reservations are **not** invented. At this SHA there is no `QuotaLedger`; tests assert MCP payloads do not contain one.

## Short deterministic campaign (CI)

The authoritative process-level suite is the grokptah-service integration test. From `crates/codegen/grokptah-service`:

```bash
cargo test --locked --test always_on_grokbot -- --test-threads=1
```

The certification-lab probe `always-on-grokbot-lifecycle-v1` is the same recipe through `grokptah-cert`. It **spawns** `grokptah-service` and is skipped unless `GROKPTAH_SERVICE_BIN` points at the built binary:

```bash
cargo build --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml --bin grokptah-service
export GROKPTAH_SERVICE_BIN="$PWD/crates/codegen/grokptah-service/target/debug/grokptah-service"
# Unset ambient XAI_API_KEY / XAI_API_BASE / GROKPTAH_TOKEN_COMMAND (offline safety).
# Reports write to gitignored evals/runs/persistent-agent-cert/
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  run --repository "$PWD" --probe always-on-grokbot-lifecycle-v1
```

Generated reports stay under the output path and are not committed. Redaction: the fake provider uses bearer `test-not-a-secret`; oracles assert that sentinel never appears in MCP JSON. Do not log `XAI_API_KEY`, `GROKPTAH_SERVICE_TOKEN`, request bodies, or route URLs.

## Restart cut points

The service test kills and relaunches the same binary against the same runtime home at:

1. `occurrence-reserved`
2. `decision-work-persisted`
3. `native-intent-persisted`
4. `run-submitted`
5. `directive-proposed`
6. `orchestration-mutation-persisted`
7. `decision-applied-pending`
8. `notification-accepted-fence-pending`
9. `terminal-run-before-settlement`

After each cut: reopen MCP, drive `ptah_tick_manager_plan` twice, assert cardinalities do not shrink and Interrupted runs have `usagePendingRequests=0`.

A bounded `/ready` poll is used after spawn. That is readiness, not a test `sleep`. Native executor and manager supervisor intervals are production (1s / 2s); tests poll MCP rather than injecting a fake clock (no public seam exists to inject `FakeClock` into the standalone process).

## Parameterized soak

Default development duration is **10 minutes**. Release duration is **24 hours**. The soak is an ignored test so CI stays short.

```bash
# 10 minutes (development)
GROKBOT_SOAK_SECS=600 cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot soak_always_on_grokbot -- --ignored --nocapture

# 24 hours (release)
GROKBOT_SOAK_SECS=86400 cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot soak_always_on_grokbot -- --ignored --nocapture
```

The soak periodically kills and respawns the real process, creates bounded autonomous plans, injects the same controlled child failure, and requires `succeeded` plus stable cardinalities. It does not require a live provider.

Record in the soak log (stdout): commit SHA, seed `always-on-grokbot-v1`, duration, restart count, cycle count, provider send count. Hash the uncommitted JSON report if you wrap this command with `grokptah-cert`.

## Remaining limits before a live-provider 24h run

- No `ProviderTransport` trait and no quota ledger at this SHA.
- Fake HTTP via `XAI_API_BASE` is the public seam; live xAI is out of scope here.
- Native executor tick is 1s; a genuine 24h live soak still needs this process-level harness plus real credentials (not supplied by this campaign).
