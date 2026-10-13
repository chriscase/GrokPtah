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
  from admitted recovery. Ten conflicting/corrupt evidence variants refuse
  three opens while retaining their original bytes.
- At 4,096 items, recovered creation remains readable through complete
  authenticated MCP list and graph traversals. Every ID appears once, both
  readers agree on order, and the largest actual response is 159,225 bytes.
  A 4,097th create is refused; two further service replays add no items.
- Creation concurrency, request conflict and canonical path conformance
  pass. Two distinct requests competing for the final capacity slot produce
  exactly one success and one replayable capacity refusal. The full new
  creation battery passes nine tests, including its child-process entry point.
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
