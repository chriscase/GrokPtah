# Continuity probe evidence (#296)

**Status:** Passing disposable offline probe
**Date:** 2026-08-16
**Production changes:** The runtime now has a first-class persistent-agent
identity/checkpoint contract, but this historical probe still exercises the
HTTP coordinator's harness-owned derivation flow.

## What was exercised

The integration test starts the existing authenticated Streamable HTTP
coordinator against an offline `AgentHost` and one disposable Git workspace.
The Node coordinator uses only HTTP `initialize`, `tools/call`, and the
run-scoped SSE `GET`; it does not import GrokPtah Rust code or call an internal
store.

The passing run demonstrated:

- six bounded runs over one workspace, with five continuation prompts derived
  from `ptah_get_run`, `ptah_get_changes`, `ptah_get_test_results`,
  `ptah_get_handoff`, and persisted harness state;
- a persisted chain ID, ordinal, parent run ID, derivation inputs, and
  SHA-256 derivation hash for every post-seed prompt;
- rechecking every stored derivation hash before the report was emitted;
- an explicitly seeded `interrupted` source, `ptah_retry_run` with a new
  `retryOf` run, and a fresh `ptah_submit_task` after that retry;
- exact request-ID replay for every mutation used by the probe: eight mutation
  receipts across submit, approve, promote, and retry;
- an isolated run whose execution promotion state was `ready` before review,
  whose source workspace was unchanged before approval, and which appeared in
  the source workspace only after explicit review, approval, and promotion;
- a dropped SSE response followed by a reconnect using `Last-Event-ID`; and
- a held live stream flooded with 6,000 bounded events. The coordinator saw
  `notifications/ptah_recovery` with `pollTool: "ptah_get_events"`, then
  reconstructed 500 durable entries through `ptah_get_events`.

The Rust test driver only controls disposable setup and the event flood. The
coordinator path itself remains HTTP-only. The flood is paced so the journal
writer keeps up while the unread HTTP body forces the bounded broadcast
subscriber to lag; this separates live-subscriber recovery from durable
journal-write failure.

## Runtime contract that worked unchanged

The probe required no production edits. Existing behavior was sufficient for:

1. bounded run admission and terminal reads;
2. durable `retry_of` lineage for an explicit restart;
3. exact idempotency receipts keyed by request ID and payload;
4. isolated worktree readiness and the existing review/approve/promote gate;
5. scoped, resumable SSE events with monotonic sequence IDs; and
6. explicit recovery signaling that names the durable read tool.

The runtime additionally persists `AgentRecord` and
`ContinuationCheckpoint` records for desktop Build turns. Those records are
validated and resumable through an explicit desktop bridge seam; this probe
has not yet been migrated to consume them, so its harness lineage remains a
useful compatibility check for the external coordinator protocol.

## Harness-invented continuity state

This historical HTTP probe still owns and persists:

- `chainId`;
- ordinal and parent-run links for ordinary continuation;
- the complete durable read bundle used as derivation input; and
- the hash envelope placed in each post-seed prompt.

The runtime's persisted `retry_of` field remains authoritative for the
explicit restart edge exercised here. For desktop Build turns, ordinary
continuation now has explicit `agent_id`, `parent_run_id`, and checkpoint
lineage in the durable orchestration store. The HTTP probe's ordinary parent
chain remains coordinator lineage until the external protocol is deliberately
extended to consume the new contract.

## Observed degradations and follow-ups

| Observation | Evidence | Runtime change required by this probe |
| --- | --- | --- |
| The historical HTTP probe does not consume runtime checkpoints | Harness state contains the five post-seed derivation records; desktop Build runtime now exposes explicit agent/checkpoint lineage | Future protocol work can add scoped agent/checkpoint reads after an authority decision; no automatic resume is implied |
| Live recovery is a signal, not automatic replay | `notifications/ptah_recovery` names `ptah_get_events` and `afterSeq`; the coordinator must poll and reconcile | None; this is the intended fail-closed contract |
| Journal retention can make an old nonzero cursor expire | Existing `ptah_get_events` reports cursor expiry instead of silently skipping; this probe starts reconciliation at durable sequence zero | None; callers must retain/reconcile from a durable checkpoint |
| Offline disposable setup emits repeated `chrome persist failed` stderr lines while the continuity checks remain green | Test output only; no MCP call failed and no probe check depended on Chrome state | No #296 production change; isolate or repair the existing test-fixture persistence path separately if its noise is undesirable |

## Reproduction

From `crates/codegen/grokptah-agent-bridge`:

```text
cargo test --locked --test mcp_continuity_probe \
  continuity_probe_is_evidence_first_and_recoverable -- --nocapture
```

The passing run completed in approximately ten seconds on the disposable
offline fixture. The test is intentionally not a production scheduler,
unattended agent, auto-resume, auto-promotion, off-box client, or Computer Use
path.
