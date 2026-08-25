# Durable admission and attempt leases

This is the contract that lets GrokPtah accept long-running agent work and
still be honest about it after a crash, a kill, or a hostile edit of its own
ledger. It covers the orchestration control plane in
`crates/codegen/grokptah-agent-bridge/src/orchestration/`.

The one-sentence version: **a receipt that says "accepted" always has durable,
sealed, private input behind it, and that input is never removed before the run
is terminal.**

## One keyed specification

Everything below hangs off a single immutable object. `AcceptanceIntent` is an
execution *specification*, and its key **is** its integrity digest, so it is
content-addressed: two specifications with the same key are byte-identical in
every execution-relevant field, and any edit — however well-formed — produces a
different key.

Six durable holders each store that key and are re-checked against it: the
**run**, the **receipt**, the **attempt**, the **lease**, the **provider send**,
and the **worker**. Agreement across them is what makes the specification
authoritative rather than advisory.

This is what defeats a *resealed forgery*. Tampering with a sealed record and
recomputing its digest produces a record that verifies perfectly — as a
different specification. It is refused not because it fails validation, but
because no holder is bound to it. Verification therefore happens at every load
that can lead to execution (`load_bound_intent`), never only at the end.

## The three objects

| Object | Where | What it guarantees |
|---|---|---|
| `AcceptanceIntent` | `inputs/<hash>.json`, mode `0600` | The exact accepted work. Versioned integrity digest over every execution-relevant field; recomputed on every load; fails closed on any mismatch. |
| `AttemptLease` | `leases/<hash>.json`, mode `0600` | The single attempt authorized to dispatch a run. Compare-and-swap acquisition, monotonic attempt numbers, owner- and attempt-scoped heartbeat and release. |
| `LiveWorker` | in memory | The authoritative registry of dispatched attempts: cancel token, nested worker/aggregator abort handles, outer supervisor join handle, and the exactly-once settlement latch. |

### Ledger I/O

Every record that can carry private execution material is read and written
through an **open directory handle**, not through a path. A path is re-resolved
by the kernel on every syscall, so checking it and then using it is a
time-of-check/time-of-use race; a handle names one inode for its whole life.

Three properties are enforced at open time rather than inferred afterwards:

* **No follow** — `O_NOFOLLOW` with `openat`/`renameat`/`unlinkat` on Unix,
  `FILE_FLAG_OPEN_REPARSE_POINT` plus an explicit reparse-point rejection on
  Windows. A link in the final component fails the open; it is never traversed.
* **Authority** — the opened inode must be owned by this effective user, must
  be closed to group and other, and must not be hard-linked elsewhere (Unix);
  must not be a reparse point (Windows). Ownership is read from the *open
  handle*, so it describes the object actually opened.
* **Containment** — names are validated single path components, and each is a
  store-generated digest, so a caller-supplied identity can never steer a write
  out of its own ledger.

Writes are private from the first byte: the temporary is created `O_EXCL` with
mode `0600` inside the same handle, written, fsynced, renamed through that
handle, and the directory is fsynced.

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

### Receipts say what is true

A submission receipt reports `queued` for **every** accepted task, immediate
execution included. At the moment the receipt is issued nothing has started: no
handle is registered, no worker has acknowledged, and no byte has reached a
provider. Reporting `running` there would be a claim about the future, and a
crash one instruction later would make it false forever.

A run reaches `running` as its worker's own first durable act, after the start
gate opens — so the transition *is* the acknowledgement, written by the only
thing that can honestly attest to it.

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

## Registration and the start gate

An attempt is three tasks: the worker, the aggregator, and the supervisor. All
three are created behind one **closed start gate**, every handle is registered
in a single registry mutation, and only then is the gate opened. Nothing can
run before teardown is able to find and abort it — the alternative is a task
that completes and settles before the registry knows it exists.

## Provider send evidence

The distinction that matters is between *not sent* and *unknown*.

| State | Meaning | Safe to attempt again? | May complete? |
|---|---|---|---|
| `known_not_sent` | Nothing was transmitted | yes | no |
| `sending` | In flight; durable before the first byte leaves | no | no |
| `uncertain` | Transmitted, or may have been; outcome never observed | **no** | no |
| `sent` | A response was observed for this exact send identity | n/a | yes |

A send identity is minted and written *before* anything is transmitted, so a
crash mid-send always leaves evidence that a send may have happened. Failures
are typed by what they *establish*, not by blame: a preflight refusal yields
`known_not_sent`, an unobserved response or a mid-flight teardown yields
`uncertain`. Transitions are forward-only, so evidence can become more definite
but never weaker.

Two rules follow, and both are enforced in code: **no implicit resend** — only
`known_not_sent` may be carried into a new attempt without a human — and **no
fake `Completed`** — a run whose work is not known to have reached the provider
is recorded `failed` with `provider_send_unconfirmed`, however cleanly the local
future returned.

## Action-time reauthorization

Admission answers "may this run?" once. Dispatch can happen much later: after a
queue wait, after a restart, after an operator revokes a scope. Every
authorization input is therefore recomputed at action time and compared against
what the specification sealed — principal, policy, capability, project,
session, persistent agent, provider, model, route, credentials, and
continuation revision — as **fingerprints**, so a drifting credential is
detectable without the credential ever being stored. Any drift refuses the
dispatch with `authorization_drift` rather than executing under authority that
no longer exists.

What is deliberately *not* sealed is conversational progress. A queued task
exists precisely so other turns can run first, so message counts and
last-updated timestamps are not authorization inputs; re-pointing a session at
another directory, archiving it, or switching its execution mode are.

## Reconcilers

Two reconcilers re-derive state from the durable ledger alone, because every
fast path can be interrupted:

* the **expired-lease reconciler** reclaims leases whose holder stopped
  heartbeating — handing any that a live worker still owns to the teardown
  owner instead, since reclaiming those would authorize a second attempt beside
  a running future; and
* the **durable-queued reconciler** re-admits queued work that no in-process
  queue is tracking.

An expired holder can never renew, release, or be mistaken for the current
attempt, so a lease that lapses is genuinely lost rather than recoverable by
coming back.

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

### One async teardown owner

Teardown has exactly one owner. Everything that wants an attempt stopped — an
explicit cancel, a deadline, a lost lease, a supervisor exit, a shutdown —
sends a request to it rather than tearing the attempt down itself.

The supervisor's `Drop` may only **fence** (signal cancellation, abort the
nested futures) and **stage** durable terminal evidence. `Drop` is synchronous,
so it can never prove the worker stopped; releasing capacity or the durable
lease from there would authorize a second attempt on the strength of an abort
*request*. Only the teardown owner, which can await, is allowed to draw that
conclusion.

Separate compare-and-swap latches gate fencing, finalization, and capacity
release, so no synchronous path can accidentally satisfy the one that matters.
Escaped work keeps both its lease and its capacity: an escaped worker holding a
slot is a bounded capacity loss, while an escaped worker whose slot was reused
is unbounded duplicate execution.

## Idempotency beyond the receipt

Receipts are pruned by retention; **tombstones are not**. A compact durable
record of every decided request identity outlives its receipt, so waiting out
the retention horizon cannot turn a refused submission back into an executable
one. A claim for a request whose receipt has been retired is refused with the
recorded outcome rather than performed.

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
- A `sent` provider record means a response was observed for that send
  identity. It does not mean the provider's own side effects are known; that is
  the provider's contract, not this ledger's.
- The bounded admission queue holds at most 32 pending runs per host. A queued
  run that cannot be re-admitted into that bound during recovery keeps its
  record and its input, and is re-admitted by a later pump or restart.
