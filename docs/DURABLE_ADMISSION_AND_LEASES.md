# Durable admission and attempt leases

This is the contract that lets GrokPtah accept long-running agent work and
still be honest about it after a crash, a kill, or a hostile edit of its own
ledger. It covers the orchestration control plane in
`crates/codegen/grokptah-agent-bridge/src/orchestration/`.

The one-sentence version: **a receipt that says "accepted" always has durable,
sealed, private input behind it, and that input is never removed before the run
is terminal.**

## The three objects

| Object | Where | What it guarantees |
|---|---|---|
| `AcceptanceIntent` | `inputs/<hash>.json`, mode `0600` | The exact accepted work. Versioned integrity digest over every execution-relevant field; recomputed on every load; fails closed on any mismatch. |
| `AttemptLease` | `leases/<hash>.json`, mode `0600` | The single attempt authorized to dispatch a run. Compare-and-swap acquisition, monotonic attempt numbers, owner- and attempt-scoped heartbeat and release. |
| `LiveWorker` | in memory | The authoritative registry of dispatched attempts: cancel token, nested worker/aggregator abort handles, outer supervisor join handle, and the exactly-once settlement latch. |

### What the seal covers

`AcceptanceIntent::digest` is computed over the intent version, the work
identity (`run_id`), the request identity (`request_id`, `payload_hash`,
`tool`), the session identity and revision, the workspace and its revision, the
agent identity and revision, the execution spec revision, the prompt (by
SHA-256 and byte length), every bound, the execution mode, the queue policy,
the retry/parent lineage, and the acceptance timestamp.

The record denies unknown fields, and its bounds are a dedicated
deny-unknown-fields type, so adding, renaming, or dropping a field is a parse
failure rather than a silently defaulted value. Permissions wider than
owner-only, a symlink in place of the record, and a path that escapes the store
root are all rejected before the file is read.

## Crash-safe cuts

Admission is cut into steps that are each individually crash-safe. After a
crash at any cut, recovery reaches exactly one of "never ran" or "runs exactly
once" — never "ran twice".

| Cut | Durable state after the crash | Recovery |
|---|---|---|
| C0 | nothing written | The request never happened; a retry is a fresh admission. |
| C1 | idempotency claim `pending` | The claim is failed on open; the request can never later execute. |
| C2 | claim `pending` + sealed intent | The intent is reclaimed as garbage (no `complete` receipt). No run is synthesized from it. |
| C3 | claim `pending` + intent + `queued` run | The run is tombstoned `admission_lost`; it never executes. |
| C4 | receipt `complete` + intent + `queued` run | Re-admitted from the intent and executed exactly once. |
| C5 | C4 + attempt lease held | The lease is released on open (exclusive ledger ownership proves the holder is gone); still exactly once, under a fresh attempt. |
| C6 | C5 + `running` run | Terminalized `interrupted`. Model work never resumes implicitly. |
| C7 | terminal run, intent still present | The intent is reclaimed as garbage; nothing re-executes. |

The accept path walks those cuts in order:

1. validate the prompt, bounds, session, and workspace;
2. claim the idempotency key (C1);
3. reserve capacity, or decide the task is queued;
4. **write and fsync the sealed private input** (C2);
5. write the run record as `queued` — always, immediate execution included (C3);
6. enqueue, if queued;
7. **complete the receipt** (C4) — only now is the caller told "accepted";
8. **acquire the mandatory attempt lease** (C5);
9. transition `queued -> running` and dispatch (C6).

Any explicit error before step 7 permanently fails the admission: the run is
tombstoned and its input destroyed in the same step, so no later recovery pass
can find anything to run. An error at step 8 or 9 cannot fail the request — the
promise is already durable — so the run is handed back to the bounded queue
with its input intact and executed later, exactly once.

## Attempt leases

Dispatch without a held lease is not possible. Acquisition succeeds only when
there is no lease, when the current one was released, or when it is expired
against its own durable heartbeat — and every success mints a new attempt id
and bumps the attempt number. A previous holder that comes back can therefore
never renew, never release the new holder's lease, and never be mistaken for the
current attempt.

Opening the ledger takes an exclusive advisory lock. Reaching that point proves
no other process owns these runs, so every lease on disk belongs to an instance
that is gone and is released rather than waited out.

## Termination and capacity

Capacity is what allows another attempt to start, so it is released only after
the previous attempt's futures are provably gone.

Teardown — expiry, explicit cancel, a lost lease, reaping, shutdown — signals
the cancel token, aborts the nested worker and aggregator, bounded-awaits the
outer supervisor (aborting and re-awaiting it if it overruns), and then waits,
within the same bound, for the worker's own liveness guard to drop. That guard
lives *inside* the worker future, so it flips when the future actually ends,
including on abort. A worker that ignores cancellation past the budget is
reported as `WorkerEscaped` and **keeps** its capacity: releasing it would let a
second attempt run beside a future that can still execute.

Exactly one exit path settles an attempt, chosen by a compare-and-swap latch.
The winner terminalizes the run, releases the lease, drops the input,
deregisters the worker, and releases capacity — in that order.

## Every outer-supervisor exit terminalizes

The outer supervisor owns a drop guard, so its `Drop` runs on a normal return,
an early return, a panic unwind, an abort landing on any await point, and
process shutdown. Under the settlement latch it either installs the terminal
record, or — if that cannot land, as on a full volume — leaves a bounded
recoverable finalization intent that the next store open replays. Retrying
persistence is bounded on purpose: an unbounded retry would hold admission
capacity for as long as the disk stays full.

If the process is killed outright, no `Drop` runs and the durable `running`
record is the backstop: the restart sweep terminalizes it `interrupted`.

## What is deliberately *not* guaranteed

- A `running` model turn is never resumed after a restart. `ptah_retry_run` is
  the explicit, separately idempotent way to carry that work forward.
- `interrupted` is a real terminal outcome from either the live reaper or the
  restart sweep. It is not a placeholder for work still in flight.
- The bounded admission queue holds at most 32 pending runs per host. A queued
  run that cannot be re-admitted into that bound during recovery keeps its
  record and its input, and is re-admitted by a later pump or restart.
