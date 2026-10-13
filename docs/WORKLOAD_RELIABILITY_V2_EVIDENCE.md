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
| `reprioritize_work`, `block_work`, `request_work_review` | `work_item_mutation`, `work_scope` | Reprioritize real caller Work/receipt/cleanup fault proof passes with one decision and exact work_scope response. Block/review cuts remain pending |
| `unblock_work` | `unblock`; operation-specific projector | Preserve its complete response, not just the Work row |
| `authorize_work_execution` | `decision`; explicit caller binding | Authorization identity and no redispatch on replay |
| `claim_work` | `claim`; lifecycle envelope and redacted receipt; replay restores existing lease authority | Existing #584 actual service crash battery passes; extend partial write/cleanup fault proof |
| `renew_work`, `link_work_run` | `work_lease_mutation`, `lease_view` | Existing #584 real caller cut battery passes; preserve expired/revoked/rotated lease behavior |
| `report_work_progress` | `work_attempt`; lifecycle envelope | Real caller red reproduced timestamp conflict; stable hashing and original-response/newer-row/legacy/refusal tests pass. Remaining child-process cuts pending |
| `release_work`, `complete_work`, `fail_work` | `work_item_attempt_mutation`, `work_attempt` | Complete/fail actual MCP timestamp conflicts reproduced and fixed; success, legacy fixture and refusal replay pass. Release and full cut coverage pending |
| `cancel_work` | `work_attempts`; lifecycle then managed authority cancellation | Real Work/receipt/cleanup fault cases replay through three host lifetimes; managed external cancellation ordering remains a separate acceptance row |
| `retry_work` | `work_item_mutation`, `work_scope` | Retry state and receipt replay across all relevant cuts |
| `approve_work` | `work_item_attempt_mutation`, `work_attempt` | Caller expected revision is hashed and checked by store; explicitly qualify binding and original result |
| `resolve_work_input` | Live permission ID; existing managed Parked/Resolving intent plus host receiver, then unbound Work lifecycle update | Has no request ID or ordinary authenticated idempotency claim. Preserve managed permission/effect authority; scope, receiver-gone, failed respond and restart tests are named below |
| Managed seal/finalization/reconciliation | Existing managed intent, finalization, quarantine and recovery protocols | Preserve #584 F1/F5 and external-effect settlement; inventory all relevant caller cuts |

Real cancellation and reprioritization write/receipt faults reproduce the
pending-to-failed receipt bug, and cancellation cleanup fault reproduces open
admission after uncertainty. The repaired lease failure/destructor now protects
matching original bindings in unresolved Work intents. Persist/commit/cleanup
uncertainty closes admission. Reprioritization Work/receipt failures also fail on
untouched merged main, using the same real caller tests (the temporary test file
was removed afterward). No unbound writer acquires a foreground request.

`work_mutation_replay_conformance.rs` has fourteen passing tests. Progress covers
original response after a newer report, exact legacy hash fixtures with changed
input refusal, and newly rejected input replay. Complete/fail each run actual
authenticated MCP success, legacy hash fixture and refusal replay. Cancel and
reprioritize each cover Work/receipt/cleanup write faults and exact original
response across three fresh host lifetimes. This is not yet the complete
operation/child-process cut matrix. Managed cancellation and apply/discard must
continue to settle at their existing external-effect boundaries.

## Adjacent creation audit

| Flow | Existing authority | Status |
|---|---|---|
| `create_manager_plan` | Schema-2 `ManagerCreationIntent`, absent plan/root priors, exact snapshots and original outer request; receipt settles before cleanup | Actual MCP Work-write crash reproduced orphan failure; process/write/cleanup/corruption/capacity proofs are in the related-creation target |
| Manager adoption/materialization, advance/tick/replan | Schema-3 mutation in the existing manager journal; prior/successor plan/Work/messages/sequence, original observations and caller response | Four actual service crash reproductions repaired; process, corruption, write fault, capacity and background proofs below |
| Manual `fire_routine` | Schema-2 `RoutineFireIntent` seals absent Work/activation priors, prior/successor routine, activation/dedupe, redacted input and original outer request | Actual MCP Work-write crash reproduced orphan failure; receipt, backoff, redaction and scheduled-absence proofs are in the related-creation target |
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

## Current qualification exception

The family cache launcher was attempted after a dry run but sccache's Cargo
compiler probe was refused in this wake, including after access grants. No
cache, socket, daemon or owner hold was deleted or changed to resolve it.
The supported direct qualification path uses task-owned `work/v2-qualification-target`,
Rust 1.92, normal debug profile, wrappers unset, `CARGO_INCREMENTAL=0`, locked/offline
and isolated homes. Strict fmt and all-target Clippy pass. Selected preservation targets pass; retain the earlier failed broad qualification and its
baseline comparisons, and require hosted checks for the completed slice.

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


## Related creation production evidence

`work_related_creation_recovery.rs` calls the real authenticated MCP manager and
routine tools. Its scheduled case calls the actual `fire_due_routines_at` writer
after creating its definition through MCP. Fixtures use isolated homes,
synthetic credentials and synthetic capacity records.

| Named test / family | Falsifiable evidence |
|---|---|
| `manager_creation_replays_after_work_write`, `routine_creation_replays_after_work_write` | Original pending receipt fails on the prior milestone plus only two cut hooks; repair returns the original result in three fresh processes |
| `related_creation_cut_matrix_replays_exact_outcomes_in_three_processes` | Manager: intent, plan, receipt, unlink, cleanup; routine: intent, routine, activation, dedupe, receipt, unlink, cleanup. Work cuts are the two original reproduction tests. Disk status is asserted before reopen |
| `pre_intent_crashes_have_no_created_work_or_guessed_success` | No admitted intent or new Work; three processes replay the orphan refusal rather than fabricate success |
| `admitted_write_and_cleanup_faults_keep_receipts_and_close_admission` | Manager Work/plan/receipt/cleanup and routine Work/activation/dedupe/receipt/cleanup faults preserve pending/complete original receipts; later MCP mutation and scheduled writer refuse; pre-intent faults remain ordinary refusals |
| `related_creation_corruptions_refuse_without_overwriting_evidence` | Nineteen variants in each family, three fresh refused processes each: snapshot, schema, owner, session, workspace, hash, input projection, failed/missing/malformed/conflicting receipt, filename, foreign Work/related record, malformed intent, valid and malformed unexpected attempts/decisions; complete JSON evidence bytes retained |
| `pending_related_receipt_conflicts_are_preserved_before_any_recovery_write` | Injected pending response/error/subject reproduced an overwrite on the first repair; all six family/field cases now refuse three processes with unchanged evidence |
| `related_creation_refuses_full_capacity_before_intent_admission` | Actual callers at 4,096 synthetic lane items leave no admitted intent, plan or activation, and a replayable capacity refusal |
| `recovered_related_creation_occupies_the_final_slot_once_and_remains_pageable` | Each actual caller occupies slot 4,096, dies after Work write, then replays in three processes; complete authenticated list/graph walks agree in ID set and order without duplication |
| `routine_non_work_and_backoff_outcomes_recover_without_an_extra_failure_increment` | Disabled and missing-agent manual callers at intent/routine/activation/receipt cuts replay exact activation, with zero Work and a failure counter incremented once |
| `routine_backoff_write_fault_leaves_its_prior_and_original_receipt_recoverable` | Routine-write fault retains the prior routine and pending caller; three processes install the successor and increment once |
| `routine_intent_and_replayed_activation_keep_existing_secret_redaction` | Neither sealed ledger nor replayed activation persists synthetic nested secrets; original hash binding survives |
| `legacy_unbound_related_envelopes_recover_without_inventing_an_authenticated_outcome` | Old-field projection fixture converges effects; unknown original caller stays failed. No invented binding or guessed successful outcome |
| `scheduled_writer_recovers_under_its_dedupe_authority_without_a_foreground_receipt` | Explicit absent request, exact original Work/activation across three processes, and no foreground request minted |
| `manual_routine_from_a_canonical_alias_replays_the_original_response_workspace` | An input workspace path alias canonicalizes under existing authorization and keeps the original canonical response display; the record itself remains in its existing canonical scope |

Initial related repair passed 3 new process tests plus manager MCP 2, routine MCP
1 and routine store 10. Expanded initial matrix passed 12/12 before the pending
receipt and canonical-alias probes. The definitive original receipt-loss red
log is `work/v2-related-creation-red-final.log`; first compile and loopback
infrastructure attempts are not reproductions. Pending-field red logs are
`work/v2-related-pending-red.log` and `work/v2-creation-pending-red.log`.
Final selected preservation passed 129/129 before the canonical creation receipt
inspection correction. The final correction separately passes 59/59: related
creation 16, ordinary creation recovery 8/conformance 2, caller replay 14,
manager MCP 2/store 6 and routine MCP 1/store 10. Strict fmt and all-target
Clippy pass on these final sources. The initial capacity proof walked every
full lane in all three replay processes; the final test retains the first full
list/graph walk and two further ID/receipt reopens without repeating that
expensive traversal. Whole-slice
hosted CI, manager adoption audit and full caller cut acceptance remain open.


The additional ordinary-creation inspector test has two real admitted caller
fixtures: a linked canonical receipt path and a conflicting owner-layout legacy
receipt. Both reproduce side effects during recovery inspection (accepting the
link or moving the legacy evidence before the hash refusal). The new canonical
inspector is shared only by new sealed creations. It refuses either fixture
across three opens without writing Work, moving evidence or replacing the link;
ordinary legacy claim/read migration remains under its original authority.
Logs: `work/v2-creation-receipt-inspection-red.log`,
`work/v2-creation-receipt-inspection-red-2.log`, and the final 59-test qualification.

The first alias probe changed the stored routine workspace and was correctly
refused by the existing exact routine scope rule. That fixture was corrected to
supply an alias as the caller input while preserving the canonical stored
record; no production authority was broadened. `work/v2-related-alias-red.log`
is a rejected fixture, not an introduced product defect.

Managed permission preservation references:
`native_executor_store::{resolve_permission_requires_parked_scope_and_updates_attempt,
resolve_stale_non_parked_and_denial_do_not_mutate_wrongly}`;
`native_executor_mcp::{resolve_work_input_requires_parked_scope,
permission_from_run_b_does_not_park_work_a, resolve_work_input_uses_real_host_pending,
resolve_work_input_fails_after_restart_without_host_pending,
resolving_dead_oneshot_does_not_unpark_on_recovery,
failed_permission_respond_then_recovery_does_not_unpark}`. The store cases pass in
the selected 46-test native target; all six focused MCP cases pass on the final sources (`work/v2-related-permission-preservation.log`). The seven idempotency/legacy compatibility library cases also pass (`work/v2-related-compatibility-final.log`). Remaining managed acceptance is still open.

The four manager mutation caller boundaries reproduced orphan receipt failure
on checkpoint `026f537001dd60292bd8cbf8871e162399b00ab4` plus only the process-cut
instrumentation (`work/v2-manager-mutation-red-2.log`). They are now implemented
through the existing manager-intents authority. The `start_verified_change`
derived `:plan` and `:advance` entry flow uses these repaired callers; the full
real Verified Change composition proof remains pending.


The final corrupt-evidence expansion adds valid and malformed unexpected attempt
and decision records to both related families (nineteen variants each). That
named test passes on the final fixture source, with the same unchanged final
production sources. The 59-test run precedes this test-only expansion, which
is separately covered by `work/v2-related-extra-evidence-final.log` and the final
all-target Clippy run. The older 129-test selected run precedes the last canonical
inspector correction; its exact phase is preserved in task-owned results.


## Manager mutation production evidence

`manager_mutation_recovery.rs` invokes authenticated production service callers,
with real child-process death, isolated homes and synthetic inputs. Its bounded
read checks call the same scoped service entry points used by MCP. The final
capacity case also walks the entire recovered lane and reads child detail over
authenticated MCP HTTP. Other caller cuts use direct service transport; existing
manager MCP and supervisor transport preservation is qualified separately. Each admitted cut or write
fault reopens three processes and compares the original sealed response,
original request identity/subject, exact Work IDs, plan/message snapshots,
sequence and retained records. Partial page counts never substitute for a whole
lane traversal.

| Named test | Falsifiable evidence |
|---|---|
| `advance_child_written_cut_replays_the_original_request`, `tick_child_written_cut_replays_the_original_request`, `replan_written_cut_replays_the_original_request`, `tick_notification_written_cut_replays_the_original_request` | Four actual original-request red cases now replay their sealed result; root/container Work stays unchanged |
| `manager_mutation_cut_matrix_preserves_sealed_responses` | Thirty-two operation/cut combinations: admitted intent, first/all children, sequence, first/all messages, plan, receipt, intent unlink and completed cleanup; three fresh process replays |
| `manager_mutation_recovery_refuses_conflicting_evidence_without_rewriting_records` | Thirty-six operation/evidence combinations: plan, child, message, counter, original receipt, missing/linked receipt, digest/schema/filename/request; three refused opens preserve bytes |
| `recomputed_manager_seals_cannot_substitute_an_original_derivation` | Changed child objective/order/prior, plan state, notification content/identity, original observation or replan input refuses even after a recomputed snapshot checksum |
| `pre_intent_manager_process_death_has_no_mutation_effect` | Four original pending requests interrupted before intent admission become replayable refusals, with no child/message/plan effect |
| `manager_mutation_write_faults_preserve_original_claim_and_close_admission` | Twenty-two admitted/pre-admission operation/write combinations; partial child batch, sequence/message, plan, receipt and cleanup; later manager/send/ack writes refuse after uncertainty |
| `background_manager_materialization_has_explicitly_no_foreground_receipt` | Eight supervisor operation/cut combinations with explicit null request/native status; recover before a new supervisor starts, no ordinary request minted |
| `original_manager_response_stays_original_after_a_newer_plan_observation` | A separate actual cancel and manager advance produce newer state; original advance/tick still returns its original response without overwriting that state |
| `manager_child_batch_capacity_is_admitted_before_any_effect` | A two-child batch fills exactly 4,096 Work after first-child process death; full authenticated MCP bounded list/graph and child-detail reads, plus two further ID/receipt reopens; overflow refuses the entire batch and leaves prior plan |
| `recovered_tick_preserves_retention_and_existing_question_identity` | Counter/message/unlink cuts retain exactly 500 messages and sequence 501; adopting an existing question preserves its original ID, TTL, sequence and ack instead of comparing a fresh server clock |

The initial four-case repaired target passed 5/5 including its child entry point.
The expanded first full matrix passed 8/8 before the observation derivation
extension. Later final-source run `work/v2-manager-final-matrix-1.log` passed
11/12, with only a background fixture assertion failing: a newly started
supervisor legitimately created decision Work after recovery. The fixture now
asserts recovery on store open before starting another supervisor. The final
focused run `work/v2-manager-final-targeted.log` passes that correction and the
new retention/question test (2/2), completing named coverage of thirteen tests
on these production sources. This is phase-specific evidence, not a claimed
single 13/13 full-target run. A clock-corruption fixture was corrected to alter
the notification clock used in the final output; altering an intermediate clock
that the notification overwrites does not substitute the outcome.

Final ordering probe `work/v2-manager-child-order-red.log` demonstrates that
recomputing the digest after reversing the child array could substitute response
order. Validation now compares that order with the original derivation.
`work/v2-manager-last-correction.log` passes the ten derivation corruption variants
and four pre-intent process cases (2 named tests); its new MCP detail assertion
used the wrong response field and failed after successful full list/graph walks.
The corrected detail assertion uses the existing `body` contract. Final capacity
MCP proof passes (`work/v2-manager-capacity-mcp-final.log`, 1/1), including the
complete 4,096-item lane and child summary. Named manager coverage is now fourteen
tests across the recorded phases, with no claimed single 14/14 full-target run.

Selected preservation completed 128/128 (`work/v2-manager-preservation-3.log`),
before the final child-order derivation correction. Final manager MCP/store/
supervisor consumers separately pass 9/9 after that correction
(`work/v2-manager-final-consumers.log`). Strict all-target Clippy passes on final
sources (`work/v2-manager-clippy-final.log`); fmt and diff checks pass. Initial
MCP preservation hit localhost bind `Operation not permitted`; current-turn
network permission permitted the isolated servers. An initial preservation
command used incorrect target names and ran no tests. These infrastructure/
command failures remain recorded and do not qualify product behavior.

The compatible family launcher dry-run still resolves the existing shared
cache namespace (`work/v2-manager-cache-dry-run.log`). The earlier repeated Cargo
compiler-probe refusal remains documented; normal Rust 1.92 direct task-owned
qualification uses locked/offline, wrappers unset and the unchanged debug profile.
No cache, socket, daemon, hold or sandbox policy was changed. Prior broad local
sandbox failures remain retained. No hosted v2 CI, draft handoff, complete caller
matrix or independent CLEAN acceptance is claimed by this checkpoint.
#520 and #521 were freshly reverified open after the checks.
