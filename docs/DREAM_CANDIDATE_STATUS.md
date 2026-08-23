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
- The logical-years certification fixture is typed with deny-unknown fields and
  remains secret-free.
- The certification now covers rollback-safe temporal history and pressure
  compaction without dropping the newly admitted fact.
- Shared desktop/hosted black-box parity is present as a fail-closed fixture.
  Candidate revisions without an independently captured immutable golden are
  rejected rather than inferred. The local sandbox cannot run its loopback
  gateway, so the candidate parity campaign still needs a host run.

## Verification recorded

- `memory::tests`: 34 passed, 0 failed.
- `memory_long_horizon` integration fixture: 1 passed, 0 failed.
- `grokptah-agent-bridge` Clippy with `-D warnings`: passed.
- Shared black-box fixture: 12 deterministic checks pass; the parity case is
  intentionally blocked here by loopback bind permission and remains open for
  host/CI evidence.

## Still required before a 100% claim

1. Wire the memory core through host-owned orchestration and manager attribution
   surfaces; these operations remain crate-private by design.
2. Capture the candidate's immutable desktop/hosted parity golden on a host
   where the service gateway can bind, then run the full authenticated fixture.
3. Complete live Grok Build quota/exhaustion evidence, least-privilege worker
   certification, isolated visual Computer Use hardware proof, packaged UI
   acceptance plus recurring expert reviews, a 72-hour operational soak, and
   the enterprise-gateway long-running review lane.

