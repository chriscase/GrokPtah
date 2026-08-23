# Always-on Grokbot process certification

This campaign proves a **bounded process smoke and one accepted-request restart fence** on a real standalone `grokptah-service` process, driven only through authenticated MCP, with a loopback fake provider that can hold a POST after acceptance. It does not call live xAI. `GROKPTAH_AGENT_OFFLINE` is not a substitute for the scripted provider.

It does **not** certify durable always-on operation, `UncertainAccept`, quota reservation, provider-attempt projection, or soak.

Base SHA: `67e29bd34dc64049432c715c93c2cef2185c63ea`.

Fixture: `crates/codegen/grokptah-service/tests/fixtures/always_on_grokbot.json` (`grokptah.always_on_grokbot_fixture.v1`, schemaVersion 2). Tests load a deny-unknown typed schema and fail closed on extra or missing fields.

Proved oracle: `interrupted_run_not_readmitted_within_window`. Base main has no durable `ProviderAttempt` / `UncertainAccept` / `RetryClass` / `QuotaReservation` projection; the fixture records those as absent. This campaign does not stamp `UncertainAttemptNotResumed` and does not treat `Run=interrupted` plus `pending=0` as quota or attempt proof.

**Next required product-head campaign:** PR #352 plus provider-attempt / quota / UncertainAccept integration.

## Proven

1. Opt-in autonomous Manager plan (`step-a` then `step-b`) advances by polling public authenticated MCP only. No `ptah_tick_manager_plan` is used to drive the DAG. A duplicate tick after `succeeded` is an idempotence negative control.
2. Exact happy-path oracle keyed to the campaign plan, not setup Runs: fixture cardinalities for `manager-decision` Work, observed `manager_proposal` Run, exactly one native Work/intent/attempt/Run and one provider POST per `step-a` / `step-b` / `step-b-fix`, all terminal runs `usagePendingRequests=0`, plan `succeeded`. Each native step is an exact relational join: one Work revision, one public Work Attempt ID, one ManagedIntent whose `attemptId` matches, one Run ID in `linkedRunIds`, one physical provider record for that semantic id. Intent work/spec/input revisions and hashes and Run `requestId` are asserted where public.
3. Same `request_id` + same payload replays the same plan id with no semantic growth. Same id + different payload returns `conflict`.
4. Scheduler-window observations (not physical cutpoints) wait for the exact target Work identity and, when a terminal state is required, that state. After a same-home restart, those identities remain unique. A linked Run with zero matching provider POSTs is `KnownNotSent` and may send once; once a matching POST exists, another POST is a resume and fails the oracle. Send equality is never inferred from `linkedRunIds`. The only deterministic physical cut is the provider Condvar barrier.
5. Two real restarts of the same home for a held accepted/no-response request: PID0 reaches the provider barrier, SIGKILL, PID1 recovery/convergence, SIGKILL PID1, PID2 identical assertions. The old MCP endpoint is dead after each kill. Semantic and physical POST count stays exactly 1. Work never returns to `queued`. Exact Work/attempt/intent/Run IDs remain stable. After each recovery, the held request's identities, unique Work cardinality, and provider posts show zero growth for two supervisor periods (2 × 2s). Session and plan counts stay unchanged. That is not a freeze of every later scheduler step on the same plan.
6. Fail-closed: invalid manager directive produces no replacement step/work; cancel / malformed / disconnect / HTTP 500 / timeout each wait for the fixture's exact public `state` + `stopCause` pair (not `completed|failed|cancelled|interrupted` as a bag), plus the observed `errorCode` and `pending=0`. CI-observed malformed/disconnect/500/timeout typed pair is `limit_reached` / `token_accounting_unavailable` (`errorCode` remains `max_total_tokens_usage_unavailable`). Cancel is `cancelled` / `token_accounting_unavailable`. Each case stays at exactly one request, no subsequent entity growth, and two restarts. Provider POST identity is the current user `Kind:` / `Objective:` header; session history and manager snapshots do not steal `step-b-fix` as a second `manager-decision`. Missing/wrong MCP bearer and outside/traversal/wrong-session/escaping-symlink/symlink-swap workspaces reject with a byte/hash-identical durable home and unchanged session/plan/Work/intent/Run cardinalities. The fake provider rejects missing/wrong expected bearer with HTTP 401 and does not count those as campaign POSTs. In-root canonicalizing symlinks are accepted by production allowlist code and are **not** claimed rejected.
7. Redaction: every MCP structured+text result is sentinel-scanned, then `scan_value_for_forbidden_data` is called on a projection that replaces public protocol identity fields and high-entropy opaque tokens (`agentId`, `displayName`, `runId`, `workId`, `workspace`, and other `*Id`/`*Hash` keys) with placeholders. Scan failures propagate; the campaign does not discard `Result`s. Bounded stderr head+tail and persisted-home UTF-8 text are scanned for bearer/API-key/live-base-URL sentinels. Artifact walks fail closed on depth, file-count, byte-size, and non-UTF8 ceilings. The fake provider records method/path/auth-acceptance/body digest/semantic id and never stores raw secrets.
8. Certification-lab probe `always-on-grokbot-lifecycle-v1` stamps only observed actions/oracles and only transitions it actually saw. Durable-read recovery compares plan id, DAG step identity (stepId/kind/objective/dependencies/assignedAgentId), and that identity hash. Live plan/step states may move `active` → `needs_replan` after SIGKILL of an in-flight step; that is not treated as a different plan.

## Explicitly unverified / out of scope

- **Proposal-only enforcement** is `unverified-pending-pr-352`. Observing `purpose=manager_proposal` is not enforcement. A malicious tool-call side-effect proof is not claimed here.
- **Internal persistence cuts** (`occurrence-reserved`, `native-intent-persisted`, and the rest of the original nine names) are `not-proven-no-production-failpoint`. This campaign uses externally observable process boundaries only.
- **ProviderAttempt / UncertainAccept / RetryClass / QuotaReservation**: absent on base main. Not synthesized from interrupted-run evidence.
- **FakeClock / deterministic scheduler**: no public clock seam exists on the standalone process. Polls are **bounded/race-controlled**, not deterministic. Broad work-materialized waits are scheduler-window observations, not cutpoints.
- **10m soak**: ignored harness, no pinned artifact. `unverified-ignored-harness-no-pinned-artifact`.
- **24h soak**: no retained pinned-head artifact. `unverified-no-pinned-head-artifact`.
- **CI one-cycle smoke** cannot claim either 10m or 24h.
- **Live xAI**: out of scope. Ambient `XAI_*` / `GROKPTAH_TOKEN_COMMAND` values are stripped before injecting loopback test credentials.
- **Desktop advisory-lock flake**: `.github/workflows/desktop.yml` already notes store tests use advisory file locks and `--test-threads=1`. That flake is **out of scope** for this child PR and is not fixed here.

## Short campaign (CI)

Ubuntu CI installs `libdbus-1-dev` and `pkg-config` before Cargo, then fmt, clippy `-D warnings`, process integration, lab units, and the actual probe. Workflow paths include `crates/codegen/grokptah-agent-bridge/**` because the service and lab depend on that crate.

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

## Restart fence (externally observable)

The deterministic physical cut is the provider POST Condvar barrier:

1. PID0: exact `step-a` POST accepted and held (no response).
2. SIGKILL; previous MCP listen address is dead.
3. PID1 ≠ PID0: Run `interrupted` with pending 0; Work `failed` not queued; POST count 1; IDs stable; zero growth of that held request for two supervisor periods.
4. SIGKILL PID1; previous endpoint dead.
5. PID2 ≠ PID1: the same recovery assertions and another two-period zero-growth window.

Scheduler-window rows (fresh home per row) are not cutpoints:

1. `step-a-work-materialized`
2. `step-a-succeeded`
3. `step-b-failed`
4. `manager-decision-succeeded`
5. `step-b-fix-materialized`
6. `plan-succeeded`

A bounded `/ready` poll is used after spawn. HTTP 200 is clean-start readiness. After SIGKILL of an in-flight Run the same probe may return 503 while supervisors record a last_error; the campaign then requires MCP initialize on the printed listen address. That is readiness, not a test `sleep`. Native executor and manager supervisor intervals are production (1s / 2s).

## Soak

CI runs **one** bounded barrier-restart cycle on one persistent home/provider/service: held `step-a` POST, two same-home SIGKILL recoveries, and parent+child resource samples (the loopback provider runs in-process, so its accept/worker threads are in the parent counts). The report records commit SHA, mode `one-cycle-smoke`, fixture schema/hash, duration, cycles, **restarts=2**, sends, maxima, redaction, SHA-256. That is not a 10-minute or 24-hour pass.

The ignored 10m/24h commands keep one campaign for the full duration, restart at unique held-request provider barriers, wait for convergence/no-resend, sample parent+child+provider and retained artifact roots, and reject a `GROKBOT_SOAK_SECS` override that does not match the mode. 24h remains unverified unless a retained pinned-head artifact path exists.

```bash
# 10 minutes (development, ignored; still not a certification artifact)
GROKBOT_SOAK_SECS=600 cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot soak_always_on_grokbot_10m -- --ignored --nocapture

# 24 hours (release retained command; not run in this PR; unverified without a pinned-head artifact)
GROKBOT_SOAK_SECS=86400 cargo test --locked --manifest-path crates/codegen/grokptah-service/Cargo.toml \
  --test always_on_grokbot soak_always_on_grokbot_24h -- --ignored --nocapture
```
