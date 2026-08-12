# Reliability Evaluations

GrokPtah has two complementary evaluation paths:

- `reliability_eval` is deterministic and offline. It exercises the shipped
  bridge API in disposable workspaces and is suitable for CI and regression
  checks.
- `live_eval` is an explicit credentialed model run. It measures coding-task
  quality and parity, but is intentionally not a CI gate.

## Deterministic Campaign

From the bridge crate:

```sh
cargo test --locked --test reliability_eval -- --test-threads=1
cargo run --locked --example reliability_eval -- \
  --out ../../../evals/runs/reliability
```

The runner uses a temporary GrokPtah home and disposable fixture directories.
It does not read the developer's real sessions, credentials, or workspaces.
The output directory is caller-selected and the report filename is fixed to
`reliability-report.json`.

The CI campaign currently covers:

| Scenario | Evidence |
| --- | --- |
| `coding_flow` | offline write, typed file-edit event, changed-file summary, completion evidence ordering |
| `queue_and_steering` | queue reorder, run-next priority, non-cancelling steering, exactly-once injection |
| `permission_deny` | permission request, deny decision, turn recovery |
| `cancellation` | live shell cancellation and cancelled terminal state |
| `restart_durability` | transcript and completion-history reload after host restart |
| `event_fanout_and_journal` | GUI/coordinator fan-out order and monotonic journal replay |
| `stale_evidence_detection` | late completion evidence classified as stale for a newer turn |

## Report Contract

The public `reliability_eval` module owns schema version 1. Each scenario
records:

- status, duration, named checks, failure reasons, and relative changed paths;
- bounded event counts and event-name inventory;
- terminal counts, turn correlation, stale evidence, and evidence ordering;
- an aggregate pass/fail/skip summary.

Reports are capped at 2 MiB. Absolute paths and `..` escapes are replaced with
`<outside-workspace>`. The contract does not include prompts, model output,
file contents, credentials, or absolute workspace paths.

## Live Model Evaluation

For real-model coding quality and parity comparisons, use the existing runner:

```sh
GROKPTAH_LIVE_EVAL=1 cargo run --locked --example live_eval -- \
  --tasks ../../../evals/tasks.json \
  --fixtures ../../../evals/fixtures \
  --out ../../../evals/runs/live/ptah.json \
  --model grok-build
```

Only run this with an authenticated, harmless disposable environment. Keep
the output directory bounded and inspect free disk before and after a campaign.
Live results should be compared using the oracle and evidence fields, not a
single success boolean. See [PARITY_EVALS.md](PARITY_EVALS.md) for the
head-to-head CLI comparison and known limitations.

## Interpreting Failures

Treat a deterministic failure as a product regression until the event trace,
workspace change summary, and completion history show otherwise. A failed
oracle or missing terminal event is different from a weak handoff. Use the
scenario ID and named check as the stable issue/PR reference; do not attach
raw prompts or workspace contents to an issue.

The campaign deliberately does not claim that offline behavior predicts live
model quality. Its purpose is to make session lifecycle, permission, steering,
durability, event, and evidence contracts cheap to verify before spending
tokens on a live run.
