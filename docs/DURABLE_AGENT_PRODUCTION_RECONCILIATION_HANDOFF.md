# Durable agent: production reconciliation handoff

> **Provenance.** This file was named as the controlling contract for the
> third durability pass, but did not exist in this repository — not on the
> working branch, not on `main`, and not on any remote ref. It is written here
> from the eight corrections stated in that request, plus the two the work
> itself uncovered, so the contract exists as
> a reviewable artifact rather than as prose in a work item. Where it records a
> decision that was made during implementation rather than handed down, it says
> so.

## Status

**Not production-durable.** Independent exact-head review of
`70829ef0e11469ab0eb58e55056778d8681bc40b` returned FAIL. This document
describes the corrections applied on top of it and, just as importantly, what
is still open. Nothing here is a release qualification, a soak result, or a
claim that the long-running agent path is safe for unattended production use.

The adversarial pass that closes out this contract found three further defects
in the corrected code — recorded below as corrections 9, 10, and 11 — which is
the expected outcome of writing the tests before the claims.

## The corrections

### 1. Keyed sealing authority

The previous pass sealed every durable record with a bare SHA-256 digest over
its own fields. That detects corruption and nothing else: an attacker who can
write the ledger recomputes the digest as easily as we can. Integrity without a
secret is a checksum.

Records are now sealed with **HMAC-SHA256 under a versioned key authority**
(`orchestration::seal`). The key lives in the OS keyring where one is usable
and otherwise in an owner-only file written through the no-follow ledger API.
Every seal names the key that produced it, so a record sealed under a key this
authority no longer holds does not silently verify under the current one.

The `digest` field survives, with a demoted job: it is a **content identity**,
not an authenticator. It says *which* work a record is, so the run, the
receipt, the lease, the send, and the worker can agree on one specification.
Authenticity is the seal's job, and the seal covers the identity too, so a
forger cannot keep a valid seal while changing which specification a record
claims to be.

Rotation retains previous keys for verification and does not reseal anything by
itself. `OrchStore::reseal_all_holders` carries every input, lease, send, and
tombstone across in one transaction, refusing entirely if any single record
cannot be carried. Retiring a key before that reseal makes the old records
unverifiable — a refusal, never a silent acceptance.

### 2. Publish-after-complete registration

An attempt's registry entry is published only once the worker, aggregator, and
supervisor handles all exist. A *reservation* claims the run id first so a
second dispatch cannot interleave, but a published entry always has every
handle, because a published entry with missing handles is one teardown cannot
fully abort.

The start gate is **cancel-aware**. It has two terminal outcomes — `open`, which
releases waiters to run, and `abandon`, which releases them to exit without
running — and the distinction is the point. A gate that is merely closed is
neither: work behind it has not started and *may still start*.

This corrects a real unsoundness in the previous pass, which treated `!started`
as quiescent. A worker parked behind a closed gate has not started and is one
`open()` away from running; calling that quiescent is how capacity gets handed
to a second attempt while the first is about to execute. `WorkerLiveness` now
distinguishes "has not started" from "can never start", and only the second
counts.

### 3. No release from synchronous `Drop`

`Drop` cannot await, so it can never prove a worker stopped. It now does
exactly two things: it **fences** the attempt, and it **records that the
outcome is unknown** (`TeardownUncertain`). It does not release the lease, the
durable input, or the capacity — each of those would authorize a second attempt
on the strength of an abort *request*, and a request is not evidence.

`OrchestrationService::shutdown().await` is the path a caller that can await
should use: it aborts, bounded-awaits, and releases only against proved
quiescence.

`TeardownUncertain` is deliberately a *positive* record. Inferring uncertainty
from the absence of a terminal record cannot distinguish "we do not know" from
"we have not looked yet", and those demand opposite behaviour. While it is
present the run keeps its lease and its capacity, restart recovery neither
recovers nor tombstones it, and dispatch refuses it.

### 4. An honest `Starting` cut

`queued -> running` hid a real interval: the lease is taken, tasks are created,
handles are registered, and only then does a worker begin. A crash inside that
window used to look like "still queued", which the ledger could not support —
the lease was already gone.

`RunState::Starting` is persisted **before the gate opens**, so the ledger
admits the attempt exists before any of it can run. `Running` follows worker
acknowledgement, written by the worker itself as its first durable act. The
accept-time audit no longer says "run started"; dispatch audits itself
separately, after both a lease and a registration exist.

### 5. Real authorization binding

The principal is the **authenticated caller** (`AuthContext::token_id`), sealed
into the specification so a queued task dispatched later still executes as the
principal that was authorized rather than as whoever is configured then. The
route fingerprint names the concrete provider id, model, wire model, endpoint,
and credential digest — so a credential rotation, a re-pointed base URL, or a
model swap is drift.

Reauthorization now runs at **two** checkpoints: before queue promotion, so a
task whose scope was revoked does not consume a capacity slot or take a lease;
and immediately before the gate opens, from inside the worker itself.

*Decision made during implementation:* conversational progress is deliberately
**not** an authorization input. An earlier attempt sealed `message_count` and
`updated_at`, which made queueing on a busy session impossible — every
unrelated turn looked like drift. What is sealed is what changes where or under
what policy the work executes.

### 6. Per-physical-request provider identity

Send identity moved from per-attempt to **per physical HTTP request**, which is
the granularity duplication actually happens at. Each request mints a stable
identity and carries an `Idempotency-Key` and `X-Request-Id` **on the wire**, so
a provider that honours idempotency can collapse a duplicate we could not
prevent.

Phases are `KnownNotSent → Sending → Sent → Responding → Settled`, with
`Uncertain` for any outcome never observed. They move forward only. A restart
reinterprets anything in flight as `Uncertain`, never as `KnownNotSent`.

The retry loop in `call_xai_agent_step` used to resend blindly on a transport
error — indistinguishable from "the provider received it, ran it, billed for
it, and we never saw the reply". Every send now consults `may_send` first, and a
resend across an unobserved outcome is **refused rather than attempted**. Only a
failure that provably never reached the socket (a connect failure) is
resendable.

### 7. One ordered receipt-and-tombstone transaction

The two records are written under a **single** store guard, so no other writer
can observe or interleave with the window between them. The seal is taken
*before* the transaction opens, because sealing is the fallible half — it fails
closed on a retired or unavailable key — and a key that cannot seal should abort
the finish with nothing written rather than half of it.

Within the transaction the tombstone is written **first** and the receipt
second. A crash between them
leaves a tombstone with no receipt, which refuses the request — the safe
direction. The reverse order would leave a receipt whose decision disappears at
the retention horizon, which is how a refused submission becomes executable by
waiting.

Tombstones carry the same keyed seal as every other holder, because a forgeable
tombstone is worth exactly as much to an attacker as a forgeable receipt.

### 8. Public truth

`durable-admission.d.ts` previously shipped **function bodies**, which is not
valid in a declaration file and fails any consumer that type-checks its
dependencies. It is now declarations only, and is type-checked by `tsc --strict`
as a gate.

The projection publishes heartbeat age, lease expiry, the route fingerprint,
teardown uncertainty and its reason, remaining bounds, retry eligibility, and
capacity fencing. `retryEligible` is computed, not inferred from `state`: a run
can be terminal and still unsafe to retry, because what is unknown is whether
its previous work ran.

The Rust type, the JSON Schema, and the TypeScript declaration are held in
lockstep by a test that fails if any of the three names a field the others do
not.

### 9. The advisory lock is not proof that work stopped

`OrchStore::open` released **every** unfenced attempt lease on the ground that
holding the exclusive advisory lock proves no other process owns these runs.
That inference is half right and dangerous. The lock proves the previous
*coordinator process* is gone. It proves nothing about the worker that
coordinator spawned: a process killed outright never reaches its fencing code,
and the children it started outlive it. Releasing on that basis hands the run
to a successor on the strength of the first attempt's silence — the same
"absence of evidence is evidence of absence" error correction 3 removed from
`Drop`.

A lease is now released at open only when something durable establishes that
nothing can be behind it: the lease does not verify (it authorizes nothing),
the run is terminal or has no record, the run is still `Queued` — which is
exactly what the honest `Starting` cut from correction 4 buys, since a queued
run's start gate never opened — or the lease has outlived its own TTL, which is
the bounded form of the same statement. Anything else keeps its lease and gains
a `TeardownUncertain` record saying this process never observed it stop.

Recovery order changed with it: runs are terminalized *before* leases are
considered, because a run's disposition is what decides whether its lease may
be released at all.

### 10. A fence is a fence in every state

`mark_unfinished_interrupted` honoured `TeardownUncertain` only for `Queued`
runs. A fenced `Running` or `Starting` run was terminalized like any other,
which contradicts this contract's own statement that restart recovery neither
recovers nor tombstones a fenced run — and, worse, freed its lease on the next
pass. Every state now checks the fence first.

### 11. A terminal run kept its private input forever

Recovery cleared the sealed acceptance intent only for runs it terminalized
*itself*. A run already made terminal by another path — an outer supervisor
staging `interrupted` on its way out being the common one — kept its input
across every subsequent restart. Nothing could dispatch it (nothing re-admits
a terminal run), so this was not a safety hole; it was a retention hole, and
the input is the private prompt. `OrchStore::open` now drops the input behind
every terminal, unfenced run, whichever path terminalized it. A fenced run is
the deliberate exception and keeps everything, because its outcome is unknown
and reconciling it may still need the input.

Defects 9 and 10 were found by the cross-process tests in
`tests/orchestration_durability_p2.rs`, which is the only place they *could*
be found: a lease exists to survive the death of the process holding it, and a
test with one process cannot observe that. Defect 11 surfaced from tightening
`shutdown_terminalizes_every_live_run` to assert that the fence, the retained
lease, and the retained input always agree with each other.

*Decision made during implementation:* that test now asserts the agreement
rather than one branch of it. Whether a synchronous `Drop` proves quiescence
is a genuine race with the abort — sometimes the worker future really is gone
before `Drop` looks, and then there is nothing to fence. Asserting the fenced
branch unconditionally was asserting who won a race, and it flaked. The fenced
branch is instead driven deterministically, across a real process boundary,
by `a_fenced_attempt_survives_the_death_of_the_coordinator_that_fenced_it`.

## What this contract does not cover

- **Soak, release qualification, and macOS CI** are out of scope and have not
  been run.
- The **journal is not yet threaded** from the orchestration layer through
  `host.rs` into the coding-agent loop. `call_xai_agent_step_journaled` exists,
  is exercised against a loopback fake provider, and enforces the resend gate;
  the production coding loop still calls the unjournaled entry point.
- **Windows** ledger records are now written with an explicit *protected* DACL
  granting only the record's owner and SYSTEM, and are verified against it on
  read — the analogue of `uid == euid && mode & 0o077 == 0`. The decision is a
  pure function (`windows_dacl_verdict`) executed by unit tests on every
  platform, including Linux CI, because that is where a logic error would be.
  The syscalls that read and install the descriptor are **compile-verified
  only**: they are checked for `x86_64-pc-windows-gnu` on every build here, and
  the behaviour tests that exercise them (`#[cfg(windows)]`) have not been run,
  because this environment has no Windows host. Treat Windows enforcement as
  unproven until they are.
- **Multi-process expiry** is now exercised against real child processes
  (`orchestration_durability_p2.rs`), covering a coordinator that dies fenced,
  one that dies inside the lease-to-`Starting` window, four successors racing
  the recovered run, a superseded attempt trying to heartbeat back in, and a
  second coordinator attempting to open a held ledger. What it does *not* cover
  is genuinely concurrent coordinators: the exclusive advisory lock makes that
  unrepresentable rather than safe, so the design has not been tested against
  a ledger on a filesystem where advisory locking is absent or advisory only
  (some network filesystems), where the lock would silently not hold.
- **Full-disk** behaviour is exercised on a real loop-mounted filesystem where
  the host permits one, and by an unwritable ledger everywhere else. Both reach
  the same write path. A ledger that fills mid-transaction can be left unable
  to record its own refusal; what is asserted is the invariant that survives
  that — a refused admission never executes, then or after the volume heals.
- **Provider-crash cuts** are exercised against a loopback fake server that
  accepts a request and dies mid-response, which is the case that produces
  `Uncertain`. They are not exercised against a real provider.
