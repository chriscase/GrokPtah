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

Every queued run that is *not* re-admitted — because its record was retired,
was never written, or is simply gone — is then retired as `interrupted` with
`error_code: "admission_lost"`. That marker is deliberately distinct from a
plain restart `interrupted`, because it is what the replay path checks.

The uncertain case is the reconcile-by-request-identity rule: a crash between
the durable record and the settled receipt leaves the client holding a *failed*
mutation (`fail_orphaned_idempotency_claims`), so executing it anyway would run
a request its caller was told did not happen. It fails closed; the caller owns
the retry, and it can never become a duplicate.

`take_recovered_admissions()` hands the reconstructed queue to the **first**
caller only. The store root is under an exclusive advisory lock, so there is
one store per ledger per process; single adoption is what stops a second
embedded control service from dispatching the same work twice.

### A receipt never outlives the work it accepted

Two rules keep a settled receipt honest.

**A write that cannot be made durable is never reported as accepted.** If the
admission record cannot be created, written, or fsynced, `save_admission`
removes any partial file, `submit_task` fails the claim, and the receipt
settles `failed`. A later retry of the same request id replays that failure; it
can never become a queued success. The run created moments earlier is failed
closed in the same path.

**A settled `queued` receipt is refused once its work is lost.** Receipts are
immutable, so recovery cannot rewrite one — instead the replay path checks the
run it names. If that run carries `admission_lost`, the replay returns a
conflict naming the run rather than the stale `queued` response, so the caller
reconciles by identity instead of waiting forever for work that will never run.
Queued work that is still queued, or that has since started, replays exactly as
before.

### Promotion and cancellation are exactly-once

`OrchStore::promote_admission` performs the whole transition under the store
lock: compare-and-set the admission out of `queued`, install the attempt lease,
then move the run to `running`, then unlink the record. A crash at any cut
leaves the admission consumed and the run non-running, which recovery resolves
to `interrupted` — never to a second dispatch. If the run write fails after the
admission is consumed, the run is failed closed as `admission_promotion_failed`
rather than left queued for a promotion that can never happen.

Cancellation marks the run terminal first (the authoritative fence), then
writes a `tombstoned` record and unlinks it. Two things make resurrection
impossible, and it is worth being precise about which does the work: the
terminal run record is the durable fence — promotion re-checks it inside the
same store lock, and recovery deletes any leftover record without re-queueing
it. The persisted tombstone covers the narrow window between that write and the
unlink, so even a crash mid-cancel leaves a consumed marker rather than a
promotable one.

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

## Invariant map

Each invariant, the mechanism that enforces it, and the test that fails if the
mechanism is removed.

| # | Invariant | Mechanism | Test |
| --- | --- | --- | --- |
| A1 | Accepted queued work survives restart with exact input and order | `AdmissionRecord` fsynced before the receipt settles; `sequence`-ordered recovery | `queued_admissions_survive_restart_with_exact_private_inputs_and_order` |
| A2 | A receipt never says accepted for work that could not be persisted | Write failure removes any partial file and fails the claim | `admission_write_failure_never_settles_the_receipt`, `failed_admission_write_leaves_no_promotable_record` |
| A3 | A settled `queued` receipt never replays work that was lost | `admission_lost` marker + replay refusal | `settled_queued_receipt_fails_closed_when_its_work_was_lost` |
| A4 | A repeated request id yields exactly one run and one record | Existing exclusive idempotency claim + exclusive-create admission | `duplicate_submit_is_exactly_once` |
| A5 | Every durable boundary fails closed on a crash cut | Recovery decision table | six `crash_cut_*` tests |
| A6 | A tampered record is never executed | Integrity digest verified on read and at recovery | `tampered_admission_is_quarantined_and_fails_closed` |
| A7 | Cancelled work never resurrects | Terminal run fence + tombstone | `cancel_then_restart_never_resurrects`, `crash_cut_after_cancellation_cannot_resurrect_queued_work` |
| A8 | Promotion consumes the record exactly once | Compare-and-set under the store lock | `promotion_consumes_the_durable_record_exactly_once` |
| A9 | Repeated restart causes no duplicate execution and no stuck `Running` | Single adoption + CAS promotion | `repeated_restart_yields_exactly_one_execution` |
| A10 | Two supervisors on one ledger cannot double-admit | `take_recovered_admissions` hands off once | `second_supervisor_on_the_same_ledger_adopts_nothing` |
| B1 | A stale or wrong owner cannot heartbeat | `(owner, attempt)` match required | `heartbeat_denies_stale_wrong_owner_and_terminal_attempts` |
| B2 | A stale or wrong owner cannot finalize | Lease re-checked before writing | `a_superseded_attempt_cannot_finalize_the_run` |
| B3 | A heartbeat never revives a terminal run | Terminal check precedes the lease check | `heartbeat_denies_stale_wrong_owner_and_terminal_attempts` |
| B4 | Expiry is deterministic and reaps only dead attempts | Persisted expiry is the only input | `expired_lease_is_reaped_and_releases_capacity` |
| B5 | A panicked or aborted worker reaches a terminal state | Supervisor inspects `JoinError` | `panicked_attempt_is_durably_interrupted_without_a_restart`, `cancelled_attempt_is_durably_interrupted` |
| B6 | A panicked worker releases admission capacity | Guard lives on the supervisor, not the attempt | `a_panicked_attempt_releases_admission_capacity` |
| B7 | Finalization retry is bounded and frees capacity | Attempt + wall-clock cap, then intent | `finalization_failure_releases_admission_capacity` |
| B8 | A bounded-out finalization is preserved, not claimed | Write-ahead intent replayed at open | `finalization_failure_preserves_replay_intent_without_claiming_success` |
| B9 | Restart resumes no uncertain attempt and leaves no zombie | All leases cleared at open; `Running` → `interrupted` | `restart_retires_every_attempt_lease` |

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

Explicitly **not** established by these focused tests:

- Always-On continuity or any 100% certification claim.
- Any multi-process or real-binary restart. Restart here means dropping every
  store handle and reopening the exclusively-locked ledger, which is faithful
  to the lock semantics but is not a process kill.
- True `ENOSPC`. The durable-write failures are injected by making the target
  directory a regular file, which produces a real `io::Error` at the same
  create/write/fsync boundary — the invariant is proven, the specific errno is
  not.
- Any desktop, TypeScript, packaged-app, or provider verification.
