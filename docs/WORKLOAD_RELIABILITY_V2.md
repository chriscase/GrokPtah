# Workload reliability v2

Base: `7ddce0d5af8b7d4ac8a3e27e839ab6a232ab141e`, the squash merge of
#584. Its tree matches the independently reviewed v1 head `d9d3a60de`.
[Main Desktop CI](https://github.com/chriscase/GrokPtah/actions/runs/38346610944)
passed on that actual merge commit. Issues #520 and #521 remain open.

## Reproduced creation gap

On the landed baseline, the authenticated `OrchestrationService::create_work`
saves its newly generated Work item and then completes its outer request
receipt. A child-process cut immediately after the save leaves one queued
revision-1 Work item. A fresh service fails the original request with
`mutation was interrupted before its durable receipt completed; use a new request_id`.
The failing `committed_creation_replays_the_original_request_after_process_death`
test calls the real service and checks the original complete response; it does
not manually construct or bind a store receipt.

The baseline reproduction used only a process-cut hook at the existing
save/receipt boundary. The repair retains that cut in the lifecycle committer.

## Durable creation invariant

Before durable intent admission, a rejected create has no Work effect and may
record its refusal. Once admitted, the lifecycle intent owns the original Work
ID, complete successor snapshot/digest and authenticated request identity.
Recovery may install that exact successor if absent, accept an identical
already-written successor, or refuse conflicting evidence. It never generates
a replacement ID or infers an original response from later Work state.

Creation uses the existing `work-lifecycle-intents` directory and commit,
receipt and cleanup path. It does not introduce another Work store. Creation
envelopes have schema version 2 and explicit `priorItem: null`; ordinary update
envelopes remain version 1 with the same serialized prior object. A version-1
record without its prior is invalid. Old binaries cannot recover a pending
version-2 creation and must not be used to downgrade a ledger with one pending.
Work item, attempt, public API and receipt schemas are unchanged.

The store serializes graph/capacity validation, pending receipt validation,
intent persistence and commit under its writer lock. It consumes the
operation-local binding before that lock, preserving v1 request ownership.
Creation recovery precedes dependent mutation recovery and generic orphan
cleanup. A recovery after the Work write does not count the item against lane
capacity twice.

An uncertain intent write or a failure after durable admission closes live
admission. The service leaves the original receipt for recovery instead of
recording an ordinary refusal from `fail_claim` or its lease destructor.
Conflicting/corrupt recovery evidence is retained and prevents successful open.
The original receipt's Work ID and response survive completion before cleanup.

## Qualification in progress

- Red: original real-service create, crash after Work write, fresh authenticated
  replay failed as an orphan while the Work item existed.
- Green: the same test returns the original response across three fresh
  service processes, one Work item, no attempts or decisions. The creation
  matrix also covers pre-intent refusal and cuts after intent, attempts, Work,
  receipt, intent unlink and completed cleanup.
- Four same-process write/cleanup faults distinguish pre-admission refusal
  from admitted recovery. Thirteen conflicting/corrupt evidence variants refuse
  three opens while retaining their original bytes.
- At 4,096 items, recovered creation remains readable through complete
  authenticated MCP list and graph traversals. Every ID appears once, both
  readers agree on order, and the largest actual response is 159,225 bytes.
  A 4,097th create is refused; two further service replays add no items.
- Creation concurrency, request conflict and canonical path conformance
  pass. Two distinct requests competing for the final capacity slot produce
  exactly one success and one replayable capacity refusal. The full new
  initial creation battery passed nine tests, including its child-process entry point.
  The canonical receipt inspection case adds an eighth recovery test; current
  ordinary creation qualification is eight recovery plus two conformance tests.
- Pending: complete source-backed production caller/response/cut matrix,
  including adjacent manager/routine/Verified Change creation protocols;
  fix demonstrated omissions without changing external-effect authority.
- Pending: full supported validation, hosted checks and separate independent
  adversarial review. Writer testing is not an independent CLEAN sign-off.

Local strict formatting/Clippy passed. The full library attempt passed 748
tests, ignored 11, and failed 11 in unchanged managed/Verified Change sandbox
tests; the runner reports `sandbox-exec: sandbox_apply: Operation not permitted`
and exit 71. This is a qualification limitation, not a successful full run.
The full integration attempt completed 40 targets: 464 passed, 104 failed and
3 ignored. The failures span Grok Build adapter/managed/host-CU, native executor
and Verified Change targets. The native fixture failure passed three isolated
reruns; its first failure raced a manual fixture claim against its live executor.
An untouched worktree at the merge commit reproduces the representative
sandbox, adapter and Verified Change failures; its native fixture passes.
This comparison identifies a local baseline limitation, not a waiver for the
full v2 checks. Hosted qualification remains required.

See [the v2 evidence matrix](WORKLOAD_RELIABILITY_V2_EVIDENCE.md) for the
source-backed caller inventory and explicit remaining acceptance work.

See `WORKLOAD_RELIABILITY_V1_EVIDENCE.md` for the six v1 repairs that this slice
must preserve. Held pages requiring explicit Refresh and newer selected detail
leading its list row remain accepted operating constraints.

## Production caller replay repairs

The actual service progress caller reproduced a conflict on identical inputs:
its payload hash included a freshly generated `updatedAt`. Authenticated MCP
completion and failure callers reproduced the same error because their adapter
creates a fresh `completedAt`. New request hashes exclude these bookkeeping
timestamps. The original response and timestamps remain sealed and replayed;
all meaningful progress/result fields, target, attempt, lease and scope remain
part of request identity. This agrees with the existing completion replay
comparison, which already excludes `completedAt`.

Existing successful legacy receipts remain replayable only when the exact old
hash can be proved from the original receipt's timestamp and caller inputs.
The repair does not use a newer Work row to reconstruct an old request. Legacy
failed receipts lack that sealed timestamp; they keep their prior conflict
behavior rather than inventing evidence. Newly rejected requests replay their
original refusal normally.

Real cancellation and reprioritization callers also demonstrate a partial
commit gap: `fail_claim` could change an admitted pending receipt to `failed`.
Receipt failure and Work write failure now leave the original sealed outcome
for recovery. Cleanup failure closes live admission even when the receipt is
already complete. The lease failure/destructor path checks unresolved existing
Work intents for the exact original owner/session/workspace/tool/request/hash;
this protects its own outcome without adopting another caller's receipt.
Malformed evidence is retained conservatively. Recovery authority remains in
the existing intent and receipt files.

The caller target now has fourteen passing tests, including real MCP success,
legacy hash fixtures and refusal replay, plus Work/receipt/cleanup write faults
for cancellation and reprioritization. Fault cases replay the exact original
response through three fresh host lifetimes, with no duplicate attempts or
decisions. These are same-process write-fault proofs with fresh host reopen;
additional child-process cuts and the remaining operation matrix are pending.

This wake's ordinary compiler-cache launcher was refused by sccache's Cargo
compiler probe. Current-turn access grants did not resolve that wrapper error.
Checks therefore use the repository's supported uncached qualification path in
a task-owned target, Rust 1.92, normal debug profile, locked/offline, wrappers
unset and `CARGO_INCREMENTAL=0`. No cache target, hold, socket or daemon was
removed. Outer sandbox checks remain intact. Strict fmt/Clippy passed; current
preservation results and remaining hosted qualification are recorded in the
evidence matrix and task-owned progress file.

The final compatibility regression also verifies that a changed payload leaves
its legacy owner-layout receipt untouched. Timestamp inspection reads without
adoption; the existing claim authority migrates the receipt only after proving
the exact original tool/payload hash. Correct original input then replays and
migrates successfully. Final caller target passes 14/14, six existing legacy /
idempotency unit tests pass, and final strict all-target Clippy passes.

The selected preservation run passed all 118 integration tests across v1
adversarial (16), manager MCP (2), native executor store (46), routine MCP (1),
creation (9), graph service (4), caller conformance (13 before the final legacy
inspection regression), workload MCP (1), authenticated pages (6) and workload
store (20). Its library retains the known local baseline: 748 pass, 11 failed,
11 ignored. This run is not green. The final non-mutating inspection is separately
qualified by the final 14 caller tests, six legacy/idempotency units and Clippy;
no full hosted v2 qualification or independent review is claimed.


## Related manager and routine creation

Actual authenticated MCP calls reproduce the same original-receipt loss for
manager roots and manual routine Work. A process exits after its Work write;
the legacy intent recovers that Work, but generic orphan cleanup then fails the
original authenticated request. Both original errors and receipt identities are
captured in the red evidence. These tests never manually claim or arm a receipt.

The existing `manager-intents` and `routine-intents` protocols now use version-2
envelopes. Manager creation explicitly has absent prior plan/root records.
Routine activation explicitly has absent prior activation/Work, plus the prior
routine snapshot. Original IDs and snapshot digests are sealed before any write.
Every existing dependent record and the original canonical receipt is preflighted
before recovery can write. An absent record or exact successor can converge;
foreign, malformed, non-regular or contradictory evidence is retained and
refused. Capacity validation happens before admission and again on recovery,
without counting a matching already-written Work twice.

Foreground calls pass their own explicit owner/session/canonical-workspace,
request/tool/hash, response shape, input projection and actor. Manager input
is checked against the sealed plan/root. Routine input retains the existing
redaction policy; its projection digest is separate from the original raw-input
hash in the authenticated receipt binding. Recovery does not reconstruct that
original hash from redacted fields. No raw payload secret or lease token is
added to the intent. Scheduled writers explicitly pass no foreground request
and retain their occurrence/dedupe authority.

All effects land before the exact original response and subject settle; durable
intent removal follows receipt settlement. Live admitted uncertainty closes
admission, protects the matching receipt from lease failure/destruction, and
prevents later manager materialization and scheduled firing. Routine failure
counters/backoff/circuit changes now seal both routine snapshots before writing,
so a recovered failure does not increment the counter again.

Version-1 unbound envelopes still decode and recover their known effects without
inventing an authenticated caller or successful receipt. The compatibility test
projects the old fields from an actual new caller's intent; it is not evidence
from an old deployed producer. Do not downgrade a ledger with pending version-2
intents: old binaries do not understand the new authenticated settlement fields.
Public Work, activation, plan, attempt and receipt schemas remain unchanged.

The pending receipt has its own invariant: no subject, response or error exists
yet. Additional red probes reproduced overwrite of injected pending outcomes
in both ordinary and related creation. Such evidence now refuses recovery
before any dependent record write. Successful complete receipts must match the
exact sealed original response/subject and already-present effects.

The new related-creation target includes real process cuts, same-process write
faults, full-lane bounded readers, corruption/pending-evidence refusal, backoff,
redaction, legacy compatibility and scheduled creation. The broader production
caller process matrix, manager adoption/materialization audit, Verified Change
composition and hosted final qualification remain pending. This is writer
evidence, and #520/#521 remain open for separate acceptance.


The final related target passes 16 tests. The selected preservation run passes
129 tests before the final ordinary-creation inspector correction, and the
correction passes 59 focused related/ordinary/caller/manager/routine tests.
Strict formatting and all-target Clippy pass. New sealed creation inspection
uses canonical regular-file evidence and cannot migrate a legacy receipt before
its proof; existing ordinary legacy lookup/claim migration remains intact.
This is a qualified local milestone, not final whole-slice or hosted acceptance.

Seven idempotency/legacy compatibility units and all six focused managed
permission MCP cases also pass. The final corruption row includes valid and
malformed unexpected attempts and decisions; this test-only expansion passes
separately after the 59-test run, with final strict all-target Clippy qualification.
The earlier broad local library/integration failures and their baseline
comparisons remain recorded; this milestone does not erase those limitations.
