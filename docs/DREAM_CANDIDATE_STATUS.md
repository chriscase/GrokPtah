# Dream Candidate Status

This note describes the isolated integration candidate on
`codex/dream-always-on-reconcile`. It is not a claim that `origin/main` is
complete or that the product has reached 100%.

## Newly certified in this candidate

- Long-horizon durable memory v2 is present with source-workspace addressing,
  project/private-agent/team scopes, host-gated critical writes, revision and
  supersession chains, expiry-aware retrieval, conflict reporting, bounded
  compaction, crash-safe commit cutpoints, replay receipts, and secret-shaped
  evidence redaction.
- The host now exposes replay-safe versioned memory writes to orchestration
  callers, and the model-facing `memory_write` schema accepts explicit
  idempotency, claim, temporal, supersession, and salience fields while keeping
  legacy writes compatible.
- The logical-years certification fixture is typed with deny-unknown fields and
  remains secret-free.
- The certification now covers rollback-safe temporal history and pressure
  compaction without dropping the newly admitted fact.
- Coordinator/worker store certification covers scoped identity, durable
  assignment, manager attribution, message acknowledgement, liveness, and
  restart-safe workload fencing. The workload MCP approval fixture already
  uses a durable Agent identity and rejects omitted or foreign claimants.
- Computer Use observation now re-checks the durable conflict-domain poison
  fence even for an already-granted Agent, so an uncertain sibling dispatch
  cannot be bypassed by a stale grant.
- Shared desktop/hosted black-box parity is present as a fail-closed fixture.
  Candidate revisions without an independently captured immutable golden are
  rejected rather than inferred. The local sandbox cannot run its loopback
  gateway, so the candidate parity campaign still needs a host run.

## Verification recorded

- Coordinator store suite: 12 passed, 0 failed.
- Orchestration library suite: 121 selected tests passed, 0 failed.
- Computer Use uncertain-domain regression: passed; bridge Clippy with
- `-D warnings`: passed after the observation-fence change.
- `memory::tests`: 34 passed, 0 failed.
- Host memory-scope suite: 6 passed, including versioned replay and payload
  conflict rejection.
- Host helper schema test: passed; bridge Clippy with `-D warnings`: passed.
- `memory_long_horizon` integration fixture: 1 passed, 0 failed.
- `grokptah-agent-bridge` Clippy with `-D warnings`: passed.
- Shared black-box fixture: 12 deterministic checks pass; the parity case is
  intentionally blocked here by loopback bind permission and remains open for
  host/CI evidence.
- The review-benchmark contract keeps its fail-closed live behavior; the
  fake-loopback quality path is also host-only in this sandbox because its
  provider must bind `127.0.0.1`.

## Still required before a 100% claim

1. Capture the candidate's immutable desktop/hosted parity golden on a host
   where the service gateway can bind, then run the full authenticated fixture.
2. Complete the independent long-running worker outcome: multi-worker crash/
   restart recovery, no duplicate execution, least-privilege production-shaped
   credentials, retained evidence, and the operational soak.
3. Complete live Grok Build quota/exhaustion evidence, isolated visual
   Computer Use hardware proof, packaged UI acceptance plus recurring expert
   reviews, a 72-hour operational soak, and the enterprise-gateway
   long-running review lane.
