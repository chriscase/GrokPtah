# Durable admission and attempt leases

Handoff note for the two release-blocking long-running-agent durability
repairs. Audited head: `8ad3be07eb27087acb67704fdf463ecb95b64505` on
`codex/external-worker-hardening-v1`.

Scope is core orchestration admission and liveness only: `orchestration/types.rs`,
`orchestration/store.rs`, `orchestration/service.rs`, plus focused tests. No
external-worker, UI/Help, swarm, provider/gateway, Computer Use, packaged-VM,
or release-workflow behaviour is changed.

## P0-A — accepted queued work survives restart

### What was wrong

Admission truth was split. `ptah_submit_task` persisted a `RunRecord` in state
`queued` carrying only a redacted 500-byte `promptPreview`, moved the real
execution input into a process-local `VecDeque`, and then settled the
idempotency receipt as `complete` with `{"state": "queued"}`. A restart flipped
the run to `interrupted` and destroyed the input, while the client's receipt
kept affirming that up to 32 tasks had been accepted. Recovery required the
caller to still hold the original prompt and to notice the state change
unprompted.

### What replaces it

One durable `AdmissionRecord` per accepted-but-not-started run, written under
the orchestration store at `admissions/<sha256(runId)>.json`:

- the complete bounded execution input,
- the execution mode and the negotiated `RunBounds`,
- queue lineage (`parentRunId`, `retryOf`) and a monotonic `sequence`,
- session, workspace, run and request identity,
- a private integrity digest over all of the above.

The record is **private store material**: `0600` on Unix, fsynced on both the
file and its parent directory, and never projected into a run, event, receipt
or capacity response. The public run projection keeps exactly the bounded,
redacted preview it already had.

Ordering is the point of the repair. `enqueue_pending` writes the durable
record and reserves the queue position *before* `submit_task` is allowed to
settle its receipt, so a receipt that says `queued` is backed by a queue entry
that is already on disk.

### Restart reconstruction

`OrchStore::open` runs `recover_admissions()` before `mark_unfinished_interrupted`,
and hands it the set of runs that must keep their queued position. Each record
resolves to exactly one outcome:

| On disk | Outcome |
| --- | --- |
| Unparsable, or integrity digest mismatch | Quarantined as `*.json.corrupt-<ts>`, counted, run fails closed to `interrupted` |
| `promoted` or `tombstoned` | Consumed marker — deleted, never re-dispatched |
| `queued`, run terminal / running / missing | Deleted; the run's own recovery state stands |
| `queued`, run `queued`, receipt **not** settled `complete` | Uncertain — deleted and counted; the caller was told the mutation failed, so this work must not run |
| `queued`, run `queued`, receipt settled `complete` | Re-admitted in `sequence` order |

The uncertain case is the reconcile-by-request-identity rule: a crash between
the durable record and the settled receipt leaves the client holding a *failed*
mutation (`fail_orphaned_idempotency_claims`), so executing it anyway would run
a request its caller was told did not happen. It fails closed; the caller owns
the retry, and it can never become a duplicate.

`take_recovered_admissions()` hands the reconstructed queue to the **first**
caller only. The store root is under an exclusive advisory lock, so there is
one store per ledger per process; single adoption is what stops a second
embedded control service from dispatching the same work twice.

### Promotion and cancellation are exactly-once

`OrchStore::promote_admission` performs the whole transition under the store
lock: compare-and-set the admission out of `queued`, install the attempt lease,
then move the run to `running`, then unlink the record. A crash at any cut
leaves the admission consumed and the run non-running, which recovery resolves
to `interrupted` — never to a second dispatch. If the run write fails after the
admission is consumed, the run is failed closed as `admission_promotion_failed`
rather than left queued for a promotion that can never happen.

Cancellation marks the run terminal first (the authoritative fence) and then
tombstones the admission. Promotion re-checks the run state inside the same
lock, so a cancelled run can never be promoted even before the tombstone lands,
and recovery deletes any leftover record.

## P0-B — `Running` is verified and reaped

### What was wrong

There was no run heartbeat, no lease renewal and no staleness reaper.
`RunProgress.updated_at` existed but nothing read it for liveness. Two failure
modes therefore persisted until a process restart:

- a finalization write that kept failing looped forever at 1 Hz while holding
  an admission slot, with exactly one audit entry ever emitted;
- a panicking turn future never reached its finalizer, so the record stayed
  `running`; `reaping_handles` discarded finished handles without ever
  inspecting `JoinError`.

### Attempt leases

Every `Running` attempt is owned by a `RunLease` at `leases/<sha256(runId)>.json`
carrying `(owner_id, attempt, acquired_at, heartbeat_at, expires_at)`.
`owner_id` is a fresh UUID per supervisor instance, so `(owner, attempt)` is
unique within a process and across restarts.

The lease is a sidecar rather than a `RunRecord` field for two reasons: the
public run projection stays byte-for-byte as bounded as it was, and the reaper
sweeps only live attempts instead of the whole ledger.

`heartbeat_run` may only ever extend the exact live attempt. It never creates a
lease, never adopts one held by another owner or attempt, and refuses outright
on a terminal run — a heartbeat cannot revive anything.

`reap_expired_leases` is deterministic: its only input is the persisted expiry,
so the same ledger and clock always reap the same set. An expired attempt
becomes `interrupted` with `error_code: "lost_worker"`, its lease is cleared,
and the service releases the host capacity it was holding. Live attempts and
terminal runs are untouched.

At open, **every** lease is deleted: the store root is exclusively locked, so a
lease found at open cannot belong to a live attempt. That is what lets restart
distinguish live (none, by construction), expired, uncertain and terminal
attempts without guessing.

### Supervisor owns capacity and inspects the join result

`spawn_run` now runs the turn in an inner attempt task and wraps it in a
supervisor task that holds the `AdmissionGuard`. The supervisor awaits the
attempt's `JoinHandle` and, on `JoinError`, durably finalizes the exact attempt
as `interrupted` with `worker_panic` or `worker_cancelled` — after checking the
lease still belongs to it, so a superseded attempt cannot clobber the run that
replaced it. Capacity is released whether the attempt returned, panicked or was
cancelled. A dedicated heartbeat task beats on a fixed interval, independent of
event volume and of the turn future.

### Bounded finalization

The retry loop is capped by attempts (`MAX_FINALIZATION_ATTEMPTS = 12`) and by
wall clock (`MAX_FINALIZATION_WALL_CLOCK = 30s`). On exhaustion it preserves the
terminal candidate as a write-ahead finalization intent — the same mechanism
`recover_finalization_intents` already replays at open — increments a stuck
counter, and releases the admission slot. It never reports the finalization as
having succeeded.

### Liveness policy

`LeasePolicy { heartbeat: 5s, ttl: 45s, sweep: 5s }` by default; several missed
beats are required before expiry, so a briefly starved runtime is not mistaken
for a dead worker. `OrchestrationService::set_lease_policy` overrides it and
takes effect from the next sweep, because the sweep deadline is recomputed each
iteration. `reap_stale_runs()` is also callable directly for deterministic
operation and testing.

## New health counters

`ptah_get_capacity` gains bounded counts only — no identifiers, no execution
input:

- `durableQueuedRuns`
- `health.stuckFinalizations`
- `health.pendingFinalizationIntents`
- `health.reapedRuns`
- `health.recoveredAdmissions`
- `health.uncertainAdmissions`
- `health.admissionIntegrityFailures`

`stuckFinalizations` and `uncertainAdmissions` are the two that should be zero
in a healthy soak; both were previously invisible.

## Known residuals

- **Dropping a control service without a process restart** releases its host
  queue slots but leaves its durable admissions on disk. Those runs stay
  `queued` until the ledger is reopened, at which point they are re-admitted
  correctly. Adopting another live service's orphaned queue would need
  cross-service ownership on the host, which is outside this scope.
- **A `Running` run with no lease is never reaped.** This is the microsecond
  window between the run record being saved as `running` and the lease being
  installed, plus any legacy record predating leases. It is deliberately
  conservative — no false positives — and restart resolves it.
- **F-03 (open-ended event windows on terminal runs) is not addressed here.**
  The reaper and the lost-attempt finalizer do set `end_seq` on the records
  they write, but `mark_unfinished_interrupted` and the queued-cancel path
  still do not. That remains open.
- **Retention** does not age out admission records; they are reconciled against
  their run at open instead, so accepted work is never expired out from under a
  client.

## Related gates

This work closes the ledger-side prerequisites for audit gates G-2
(uncertain-send safety) and G-3 (capacity liveness). Neither gate is *met* here
— both need the long-horizon soak campaign, which is unchanged and unrun by
this work.
