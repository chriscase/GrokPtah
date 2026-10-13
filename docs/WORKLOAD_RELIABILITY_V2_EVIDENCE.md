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

| Caller | Bound response / transaction authority | Production proof |
|---|---|---|
| `create_work` | `work_attempts`; schema-2 absent-prior lifecycle intent, original ID/snapshot/receipt | Creation process, negative, capacity, MCP and concurrency proofs above |
| `assign_work` | `work_item_mutation`, `work_scope`; legacy assign lifecycle envelope | Twenty-caller process/fault matrix below; assign creates no decision |
| `offer_work`, `reassign_work` | `decision`; Work/decision envelope | Full original Work + decision, all admitted cuts and applicable writes |
| `accept_work`, `decline_work` | `worker_identity_mutation`, `decision_scope` | Full response includes session/workspace and attributable acting/assigned Agent metadata |
| `reprioritize_work`, `block_work`, `request_work_review` | `work_item_mutation`, `work_scope`; Work/decision envelope | Full original scope response, one original decision, repeated process replay |
| `unblock_work` | `unblock`; compact operation-specific projector | Exact compact Work/revision/provenance/session response, not a full-row substitution |
| `authorize_work_execution` | `decision`; explicit original binding | Original authorization decision; no dispatch or new managed authority from receipt replay |
| `claim_work` | `claim`; lifecycle envelope and redacted receipt | Process/write/cleanup cases; restores the same original lease in memory and persists no raw token; v1 adverse lease proofs remain |
| `renew_work`, `link_work_run` | `work_lease_mutation`, `lease_view` | Original attempt/scope response; both change the attempt and intentionally leave Work unchanged; actual run-link scope gate remains before replay |
| `report_work_progress` | `work_attempt`; lifecycle envelope | Process/fault matrix plus original-response/newer-row and legacy timestamp proofs |
| `release_work`, `complete_work`, `fail_work` | `work_item_attempt_mutation`, `work_attempt` | Process/fault matrix plus actual MCP completion/failure success/legacy/refusal proofs |
| `cancel_work` | `work_attempts`; lifecycle then managed authority cancellation | Full original attempt array for ordinary Work; managed external cancellation remains under its separate effect authority |
| `retry_work` | `work_item_mutation`, `work_scope`; lifecycle envelope | Original retry state and scope, no extra attempt on replay |
| `approve_work` | `work_item_attempt_mutation`, `work_attempt` | Ordinary approval-gated completion across all cuts/writes; candidate approval/apply remains a distinct managed boundary |
| `resolve_work_input` | Live permission ID; managed Parked/Resolving intent + host receiver, then unbound Work update | No request ID/ordinary claim; existing six actual MCP permission preservation cases and native store cases are named below |
| Managed seal/finalization/reconciliation | Existing managed intent/finalization/quarantine/recovery authority | Preserve F1/F5 and external-effect settlement; full final supported/hosted qualification remains pending |


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
response across three fresh host lifetimes. The additional production process target now covers the twenty ordinary
update callers listed below. These earlier fault tests remain complementary
legacy/MCP evidence. Managed cancellation and apply/discard must
continue to settle at their existing external-effect boundaries.

## Adjacent creation audit

| Flow | Existing authority | Status |
|---|---|---|
| `create_manager_plan` | Schema-2 `ManagerCreationIntent`, absent plan/root priors, exact snapshots and original outer request; receipt settles before cleanup | Actual MCP Work-write crash reproduced orphan failure; process/write/cleanup/corruption/capacity proofs are in the related-creation target |
| Manager adoption/materialization, advance/tick/replan | Schema-3 mutation in the existing manager journal; prior/successor plan/Work/messages/sequence, original observations and caller response | Four actual service crash reproductions repaired; process, corruption, write fault, capacity and background proofs below |
| Manual `fire_routine` | Schema-2 `RoutineFireIntent` seals absent Work/activation priors, prior/successor routine, activation/dedupe, redacted input and original outer request | Actual MCP Work-write crash reproduced orphan failure; receipt, backoff, redaction and scheduled-absence proofs are in the related-creation target |
| `prepare_verified_change` | Readiness/projection | Does not create Work and is not a creation receipt gap |
| `start_verified_change` | Original derived `:plan`, `:advance`, `:authorize` callers, then established managed workflow | Real readiness/service process matrix proves creation/materialization/authorization composition; dispatch is held at the existing test boundary, external effects retain their separate qualification |
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
#521 remains open for complete supported/hosted qualification, managed external
effect acceptance, and independent review. The ordinary and adjacent creation
matrices are complete as recorded below. Neither issue is closed by this slice;
a draft and writer testing do not provide independent issue-level acceptance.

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
derived `:plan` and `:advance` entry flow uses these repaired callers; the real
creation/materialization/authorization composition proof is recorded below.


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


## Ordinary mutation receipt admission and recovery

`work_caller_process_conformance.rs` uses the actual authenticated service for
all twenty ordinary update callers. Each fixture creates its Work through the
real service and uses real offer/accept/claim/block/complete/fail preludes when
needed. The second scoped worker and linked Run are synthetic typed fixtures;
link still passes the production run scope gate. Fixtures stop unrelated
background writers and set their explicit ordinary Work policy to forbid managed
execution. These tests never manually bind a store request or call a provider.
They derive synthetic original lease credentials from immutable attempt identity
in memory, without putting raw tokens in fixture or ledger JSON.

Two actual original-receipt corruption probes reproduced recovery writing the
decision/item or attempt/item before refusing a missing canonical request:
`work/v2-callers-receipt-preflight-red.log`. Ordinary update admission and recovery
now inspect the canonical regular-file receipt and original scope/tool/hash,
refuse conflicting pending fields or completed outcomes before dependent writes,
and validate all present effects before accepting an already completed receipt.
Generic legacy claim/read migration remains under its existing authority;
recovery inspection cannot move a conflicting legacy file or follow a receipt
link. Apply/discard retain their existing settlement predicate and sealed
external-effect authority.

A separate actual assign probe showed settlement dropping the `Some(work_id)`
subject the public caller supplies (`work/v2-callers-subject-red.log`). Known
ordinary Work tools now settle their original named Work subject from the sealed
binding/response. Existing completed v1 receipts with an absent subject remain
readable and replay byte-for-byte when their original outcome/effects match;
they are not rewritten into new evidence. Failed/malformed/foreign receipts do
not gain that compatibility.

| Named test | Contract |
|---|---|
| `every_remaining_caller_replays_the_public_response_after_receipt_write` | All twenty declared public response shapes replay exactly, including compact unblock, scoped worker decision, lease view, attempt array and redacted claim |
| `remaining_production_callers_replay_each_admitted_process_cut` | One hundred operation/cut cases: intent, decision/attempt, Work stage, intent unlink and cleanup; three fresh-process replays, no extra revisions/records or lease minting |
| `pre_intent_production_caller_cuts_preserve_the_prior_records` | Twenty pre-admission process cuts leave prior Work/attempt/decision bytes unchanged and become stable original refusals |
| `all_remaining_callers_distinguish_pre_admission_refusal_from_live_recovery` | Ninety-six applicable operation/write cases; before-intent refusal versus admitted attempt/decision/item/receipt/cleanup failure; live admission closes only after uncertainty |
| `decision_caller_recovery_refuses_a_conflicting_original_receipt_before_writes`, `lifecycle_caller_recovery_refuses_a_conflicting_original_receipt_before_writes` | Twelve receipt evidence variants per family, three refused opens each, bytes and links preserved |
| `work_caller_receipt_identifies_its_original_named_work` | Lifecycle, decision and progress receipts retain the named Work subject supplied by the caller |
| `legacy_completed_work_receipts_without_subject_replay_without_rewriting` | Three response families preserve the original completed no-subject receipt bytes across three fresh processes |

Every successful replay also rejects changed payload/revision and, on the first
replay, wrong target/session/workspace without modifying the original receipt.
Bounded list and graph service reads agree on the recovered Work ID. Existing
v1 service request races, unbound writer, expired/revoked/rotated lease, full
capacity/detail and quarantine proofs remain separate acceptance evidence.
The new claim response restores the original token only to the caller; its
receipt and all ledger JSON stay redacted.

Renew and link perform no Work-item write, so their fault matrix targets the
attempt, receipt and cleanup rather than injecting an unused item fault. Their
process Work-stage cut still checks that the unchanged Work snapshot remains
consistent with the changed attempt. Already-terminal same-result/same-reviewer
fast paths that change no Work/attempt/decision are genuinely read-only outcomes
plus one new receipt; no transaction is admitted before that receipt. Worker
presence/heartbeat changes are outside the Work-mutation transaction inventory.
Managed permission and source apply/discard are not ordinary request replay rows.

The seventeen-update version passes its complete nine-test target including
83 applicable live faults (`work/v2-callers-final-matrix-4.log`). The expanded
twenty-caller target passes its eight process/response/pre-admission/compatibility
cases (`work/v2-callers-twenty-matrix-2.log`); its remaining live-fault fixture
initially targeted a Work write that renew/link do not perform. The corrected
96-case live-fault test separately passes
(`work/v2-callers-twenty-live-faults-final.log`, 1/1). These are exact source phases,
not a claimed single expanded 9/9 full-target run. Earlier fixture compile errors,
a missing renewal prelude, and retry's intentional automatic-retry policy are
recorded as fixture/iteration failures; the exact red logs above identify the
demonstrated production failures.

Selected preservation passes 121/121 on final production sources
(`work/v2-callers-preservation-final.log`; exact rows are in task-owned results).
Expanded strict all-target Clippy passes (`work/v2-callers-expanded-clippy.log`).
Related/ordinary creation and manager recovery pass 38/38
(`work/v2-callers-related-preservation.log`), including the first complete 14/14
manager mutation target. Seven compatibility/idempotency library cases pass
(`work/v2-callers-compatibility-final.log`), and six actual managed permission
MCP cases pass (`work/v2-callers-permission-preservation.log`). Prior broad local
sandbox failures remain retained. Derived Verified Change composition and final
whole-slice supported/hosted qualification plus one draft handoff remain pending.
Both issues stay open; this remains writer testing.


## Derived Verified Change creation proof

`tests/verified_creation_recovery.rs` passes its complete five-test target
(`work/v2-verified-matrix-3.log`) on the final fixture source. No further
production repair was needed: the manager creation/mutation and ordinary
receipt preflight changes compose with the actual `start_verified_change`
entry point. The original service derives its own request IDs/hashes, resolves
its real profile and readiness, and writes its own authority/envelope. The test
never arms an operation-local binding by hand.

| Named test | Falsifiable scope |
|---|---|
| `derived_manager_creation_and_materialization_replay_original_receipts` | Twelve admitted component cuts, three fresh replays each; original sealed/completed plan and child bodies, one root/child pair, exact original derived receipts |
| `derived_authorization_replays_without_launching_or_minting_attempts` | Five decision cuts, three fresh replays each; original decision/body/subject and bound authority/envelope, no attempts/Runs/managed intents |
| `derived_pre_admission_cuts_do_not_create_successors` | Three component pre-intent cuts preserve zero, one or two prior Work records respectively; original pending request becomes a stable refusal |
| `changed_profile_after_a_creation_cut_refuses_without_new_work` | Admitted original root settles on open; changed profile conflicts with its original derived hash without creating a child or attempt |

Each success sequence also refuses a changed objective without rewriting the
original outcomes. The first replay walks authenticated `ptah_list_work_page`
and `ptah_get_work_graph_page` with limit1, follows `continuation` to `complete`,
and reads `ptah_get_work_detail` section `summary`. Complete traversals agree on
the two unique recovered IDs, and actual JSON pages stay below 32 KiB. Two
further replays preserve the entire receipt/Work/plan/decision inventory bytes.
The source Git head and bytes remain unchanged; the synthetic CLI records only
`inspect --json`, never worker launch.

This fixture uses the existing compatibility lease resolver and real profile
revision1 from the production workflow fixtures. It does not qualify production
private confinement revision2 or provider/apply/discard. The existing drive hold
and stopped background writers isolate the prefix before dispatch, without
altering readiness, Work policy, managed authority or external settlement.
The generic manager unlink hook first fires in creation here; materialization
cleanup uses the separate post-commit cut plus the standalone manager unlink proof.

Retained fixture iterations include an unavailable control-server shutdown
method, incorrect authorization response projection and page/detail field names.
They were corrected to the existing contracts before the five-test green run;
none are claimed product reproductions. Localhost bind initially returned EPERM;
current-turn network permission allowed the isolated MCP servers. No sandbox
policy, compiler cache, daemon/socket, owner hold or production credential changed.

## Final whole-slice qualification

Final bridge strict all-target Clippy passes (`work/v2-final-clippy.log`).
Desktop typecheck and all 61 files / 459 tests pass on the unchanged F3/F6 consumers
(`work/v2-final-desktop-typecheck.log`, `work/v2-final-desktop-tests.log`).
The complete supported bridge run uses `--no-fail-fast` to retain every target's
result, normal Rust1.92 debug, locked/offline, task target and isolated home.
Its terminal results are below; hosted qualification remains required.
Earlier broad sandbox failures are retained above, not relabeled as success.


The final complete bridge run ends exit101: library: 748 passed / 11 failed / 11 ignored,
46 integration targets: 526 passed / 103 failed / 3 ignored, and one ignored doctest. All 68
new named tests pass in the seven recovery targets. The twenty-caller target now
has a complete 9/9 final-source run; the earlier precise phase qualifications
remain historical evidence. Manager mutation 14/14, ordinary creation 8+2,
related creation 16/16, MCP/legacy caller 14/14 and composed VerifiedChange 5/5
pass on the same production sources. The five failed targets retain the local
confinement limitation: library 11, adapter 11, managed executor 1, self-host CU1,
Verified Change 90. This is not full green or a blanket untouched-main proof for
every failed case; prior representative main reproductions remain separate.

Final headless service fmt/strict Clippy and complete 21/21 pass
(`work/v2-final-service-clippy-corrected.log`,
`work/v2-final-service-tests-corrected.log`). Its initial two complete runs each
passed20 and failed the journal-flood restart case; untouched main's complete
conformance target also failed that exact case (`work/v2-final-service-baseline-full.log`),
although a focused main run passed. The test-only `flood_journal` now observes
appended journal sequence after each 32 events (and final partial batch), with
an explicit no-persistence-error assertion and bounded wait. It retains all
5,000events, expiry, disconnect, replay and restart assertions. The queue capacity of 256, journal
limits, fail-closed shutdown/lock retention and production code are unchanged.
This fixes the fixture's timing assumption, not the Work transaction authority.

Gateway fmt/strict Clippy and 17 tests including doctest pass
(`work/v2-final-gateway-{clippy,tests}.log`). Desktop 61 files / 459 tests and typecheck
pass. Task-owned terminal result rows and original failure logs are retained.
The draft's exact-head hosted workflow and independent review remain the next
acceptance gates. Issues #520 and #521 remain OPEN by explicit instruction.
