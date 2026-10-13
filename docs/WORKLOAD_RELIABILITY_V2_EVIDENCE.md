# Workload reliability v2 evidence and remaining acceptance

This is an implementation record, not an independent adversarial sign-off.
The starting commit is `7ddce0d5af8b7d4ac8a3e27e839ab6a232ab141e`.
The [main/push Desktop run](https://github.com/chriscase/GrokPtah/actions/runs/38346610944)
succeeded on that exact merge of #584. Issues #520 and #521 stay open.

## Creation evidence

The tests below live in `crates/codegen/grokptah-agent-bridge/tests/`.
They call the production authenticated service; the capacity read test also
uses the actual MCP HTTP transport and measures complete response bodies.

| Test in `work_creation_recovery.rs` | Assertion | Current result |
|---|---|---|
| `committed_creation_replays_the_original_request_after_process_death` | A Work-written/receipt-pending process cut returns the original response and Work ID across three fresh services | Failed on landed base with only a cut hook; passes with repair |
| `creation_cuts_replay_the_sealed_successor_in_three_fresh_processes` | Cuts after intent, attempts, Work, receipt, intent unlink and cleanup converge exactly once; inspect disk before restart | Passed |
| `pre_intent_process_death_has_no_work_effect_and_replays_refusal` | No admitted intent means no Work, and three restarts replay refusal | Passed |
| `creation_write_faults_preserve_admitted_receipts_and_close_live_admission` | Intent-write refusal versus Work/receipt/cleanup uncertainty; only admitted uncertainty closes admission and preserves recovery | Passed |
| `creation_recovery_refuses_conflicting_evidence_without_writing_work` | Snapshot, owner, workspace digest, payload, failed/missing/corrupt receipt, schema, filename and foreign Work retain evidence across three refused opens | Passed |
| `recovered_creation_at_capacity_remains_pageable_over_authenticated_mcp` | Real final-slot create, interrupted after Work write; lossless 4,096-item list/graph, detail, overflow refusal and repeated replay | Passed; largest actual body 159,225 bytes |

The two `work_creation_conformance.rs` tests add duplicate-request and distinct
same/different-lane races, receipt ownership, changed-payload refusal, mismatched
session/workspace refusal, canonical paths, and competing final-slot creates.
Both pass, and the seven-test recovery target also passes after adding exact
summary and objective assertions through `ptah_get_work_detail`. Distinct requests rendezvous at the
existing operation-local binding seam; duplicate requests rendezvous before
claiming so the duplicate must replay rather than enter a second transaction.

## Production caller inventory

Source authority is `src/orchestration/service.rs` and `store.rs` in the bridge.
Public mutation requests start in `begin_work_mutation` / `begin_idempotency`,
scoped by authenticated owner, session and canonical workspace, with tool,
request ID and payload hash. Caller scope validation precedes receipt lookup.
The table identifies the operation-local response binding; shared machinery
alone is not complete proof of every caller/cut combination.

| Caller | Bound response / transaction authority | Remaining proof or source finding |
|---|---|---|
| `create_work` | `work_attempts`; schema-2 absent-prior lifecycle intent, original Work ID, snapshot digest and receipt | Creation proof above; finish additional negative and concurrency cases |
| `assign_work` | `work_item_mutation`, `work_scope`; assignment decision/Work mutation envelope | Map real caller cuts and original scope response |
| `offer_work` | `decision`; Work mutation envelope | Real caller cut/replay matrix |
| `accept_work`, `decline_work` | `worker_identity_mutation`, `decision_scope` | Identity, revision, receipt and original response at each cut |
| `reassign_work` | `decision`; Work mutation envelope | Prior and successor assignment/decision evidence at each cut |
| `reprioritize_work`, `block_work`, `request_work_review` | `work_item_mutation`, `work_scope` | Exact caller payload/revision and response proof |
| `unblock_work` | `unblock`; operation-specific projector | Preserve its complete response, not just the Work row |
| `authorize_work_execution` | `decision`; explicit caller binding | Authorization identity and no redispatch on replay |
| `claim_work` | `claim`; lifecycle envelope and redacted receipt; replay restores existing lease authority | Existing #584 actual service crash battery passes; extend partial write/cleanup fault proof |
| `renew_work`, `link_work_run` | `work_lease_mutation`, `lease_view` | Existing #584 real caller cut battery passes; preserve expired/revoked/rotated lease behavior |
| `report_work_progress` | `work_attempt`; lifecycle envelope | Source hashes fresh `WorkProgress.updated_at` before lookup. Suspected replay mismatch; reproduce through actual caller before fixing |
| `release_work`, `complete_work`, `fail_work` | `work_item_attempt_mutation`, `work_attempt` | Per-caller original response and failed-commit receipt proof |
| `cancel_work` | `work_attempts`; lifecycle then managed authority cancellation | Reproduce partial receipt/cleanup errors; preserve cancellation authority ordering |
| `retry_work` | `work_item_mutation`, `work_scope` | Retry state and receipt replay across all relevant cuts |
| `approve_work` | `work_item_attempt_mutation`, `work_attempt` | Caller expected revision is hashed and checked by store; explicitly qualify binding and original result |
| `resolve_work_input` | Synchronous service entry; deeper audit pending | Classify its actual durable boundary before including or excluding |
| Managed seal/finalization/reconciliation | Existing managed intent, finalization, quarantine and recovery protocols | Preserve #584 F1/F5 and external-effect settlement; inventory all relevant caller cuts |

After partial commit, ordinary lifecycle callers still reach `fail_claim` and
the lease destructor. Creation now explicitly preserves its admitted pending
receipt. The remaining callers need real service fault tests before changing
shared lease behavior. A failed outer call cannot honestly erase an admitted
mutation's recoverable outcome. Cleanup failures also need admission checks.

## Adjacent creation audit

| Flow | Existing authority | Status |
|---|---|---|
| `create_manager_plan` | `ManagerCreationIntent` seals root Work and manager plan; outer service receipt completes afterward | Potential equivalent receipt gap. Reproduce using actual service; extend this protocol rather than replace it |
| Manager adoption/materialization | Existing manager plan, graph and Work writers | Full admission/replay audit pending; no generic creation shortcut |
| Manual `fire_routine` | `RoutineFireIntent` seals routine/activation/Work/dedupe; outer service receipt completes afterward | Potential equivalent receipt gap; preserve routine dedupe and scheduled/background callers with explicit absence of foreground binding |
| `prepare_verified_change` | Readiness/projection | Does not create Work and is not a creation receipt gap |
| `start_verified_change` | Calls `create_manager_plan` with derived `:plan` request ID, then established managed workflow | Manager creation repair must compose with this entry point; preserve readiness and authorization |
| Verified Change apply/discard | Existing sealed admission, source effect, cleanup and managed evidence | Must settle at the external-effect boundary, never from an intermediate Work snapshot |

## Qualification and issue disposition

Strict bridge fmt and all-target Clippy passed for the final creation milestone,
including both new test targets.
The complete library attempt is not green: 748 passed, 11 ignored, 11 failed
in unchanged managed/Verified Change sandbox tests. The local runner reports
`sandbox-exec: sandbox_apply: Operation not permitted` / exit 71. Assertions and
sandbox policy remain intact. The full integration run completed 40 targets: 464 passed, 104 failed,
3 ignored. Failed targets were `grok_build_adapter` (11),
`grok_build_managed_executor` (1), `native_executor_mcp` (1),
`self_host_cu_dogfood` (1) and `verified_change_workflow` (90).
The native failure occurs in `seed_admitted_work` when its manual claim races
the live executor; the same built test passes three separate reruns. These
reruns do not erase the original failure. An untouched worktree of `7ddce0d5a` reproduces
`managed_os_confinement_blocks_host_data_oracle_writes_and_other_ports`,
`valid_complete_advisory_result`, and
`admission_pending_successor_cannot_inject_response_or_error` with the same
failures; the native fixture passes there. This is a representative baseline
comparison, not full qualification of all failed cases. Hosted qualification
and any remaining failure disposition are still required.

The v1 adversarial suite passed all 16 tests with the initial v2 changes,
including 4,096 nodes/16,384 edges, maximal attempt detail, operation-local
request races and durable quarantine refusal. The existing authenticated page target passes 6/6; graph authority/service
pass 25/25 and 4/4, and workload store passes 20/20. Manager and routine targets
pass as recorded in the task logs. Managed/Verified Change qualification remains
subject to the failures above. Desktop F3/F6
sources are unchanged; their final landing qualification passed before v2.

#520 still requires separate issue-level acceptance. Its read contract is
implemented by #584; this slice adds recovered-write/public-read composition
evidence without claiming that partial page counts are whole-lane totals.
#521 remains open for the unfinished caller and adjacent-creation matrix,
negative/replay checks, complete supported qualification and independent review.
A draft PR is not yet prepared; this document intentionally records pending rows.
