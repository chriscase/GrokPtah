# Verified Change Workflow v1 evidence

Qualification class: fixture and local CI. Not a live provider run. Not a packaged, notarized, or Computer Use claim.

Environment: macOS 26.3, arm64, rustc 1.92.0. Fake Grok CLI and disposable git repositories. No operator PATH, HOME, or GitHub token is forwarded to the child. The published projection is scanned for the fixture credential and it is absent.

The live exit gate was not run. No explicit live-use grant was present. The ignored managed-executor live test was not executed. Issue #504 and PR #560 remain the earlier DOGFOOD.txt demonstration, not this result.

## G1

Operator prepare and start are `OrchestrationService::prepare_verified_change` and `start_verified_change`. They create one manager plan and one Work item on the existing ledger, assign the existing agent, and authorize execution. Desktop commands `verified_change_prepare` and `verified_change_start` call those methods. The Work board form renders repository, revision, agent, host, scope, executor, model, CLI version, limits, required checks, approval, readiness reasons, and phases.

Paths: `crates/codegen/grokptah-agent-bridge/src/orchestration/service.rs`, `crates/codegen/grokptah-agent-bridge/src/mcp_control.rs`, `desktop/src-tauri/src/commands.rs`, `desktop/src/components/VerifiedChangePanel.tsx`, `desktop/src/components/WorkBoard.tsx`.

The operator entry is the desktop Work board and the same methods on the MCP control plane: `ptah_prepare_verified_change`, `ptah_start_verified_change`, `ptah_verified_change_status`, `ptah_apply_verified_change`, and `ptah_discard_verified_change`. Desktop commands are `verified_change_prepare`, `verified_change_start`, `verified_change_status`, `verified_change_apply`, and `verified_change_discard`. An empty agent id resolves to the session agent, so Prepare is available before any work item is selected. After start, the board refreshes status until the worker leaves `running` or `leased`, and Refresh review loads the settled diff and check results. Apply and discard require the exact `candidateDigest`. The managed executor is installed only when `GROKPTAH_MANAGED_GROK_EXECUTABLE` and its sibling settings are present. The child still receives no GitHub push credential.

Command: `cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --test verified_change_workflow -- --test-threads=1`

Result: `multi_file_repair_is_red_then_green_and_apply_is_separate` passed. Prepare and the immediate start projection both assert repository id, the full source revision, agent id, execution host, both allowed files, executor, distinct model key and CLI version `1.0.5`, limits, approval required, and zero workers dispatched by readiness.

Desktop process: the debug `grokptah-desktop` binary was started twice. Each process listened on its MCP control plane. Prepare and start were called over that plane against a fake CLI and a disposable two-file repository. Both runs returned the same outcome: readiness ready, CLI `1.0.5` with contract `inspect-json-v1`, model key `grok-4.6`, workers dispatched 0, provider invocations 0, execution host `desktop`, allowed files `src/ledger.rs` and `src/report.rs`, required check `balance-regression`, max rounds 8, approval required, and exactly one admitted attempt in state `running`. The admission snapshot's `checksPassed` is false because the worker had not settled yet. The workflow test shows the same path reaches `checksPassed` after settle. The projection did not contain the fixture credential or the control-plane token.

Desktop, from `desktop/`: `npm run typecheck` exited 0. `npm test` exited 0 (58 files, 428 tests). `desktop/src-tauri` `cargo test --locked --lib -- --test-threads=1` exited 0 (50 passed). The Work board test prepares with an empty agent id, refreshes a settled diff, and applies the displayed candidate digest.

## G2

Readiness runs the configured executable as `inspect --json` with an empty environment. It records `grokVersion` and contract `inspect-json-v1`. It does not send a prompt. `workersDispatched` and `providerInvocations` stay 0. Read-only mode and a non-macOS platform are not ready. A missing executable is not ready. Required-check executables are checked as files. Secret environment names are rejected.

Paths: `crates/codegen/grokptah-agent-bridge/src/verified_change.rs`.

Result: unit tests `readiness_missing_cli_dispatches_nothing` and `readiness_refuses_readonly_and_non_macos_without_spawning_work` passed. The workflow test's linux and read-only prepares dispatch no intents.

## G3

The production path is managed-work admission of `GrokBuildIsolatedReview` through the existing supervisor. The fixture repairs `src/ledger.rs` and `src/report.rs` together. The oracle script lives outside the checkout. It fails on the pre-repair tree and passes only when both files carry the coordinated repair. The oracle bytes are unchanged after the run. The child is not given a publish remote. When required checks are present, the source workspace is not modified until a separate apply.

Result: the workflow test asserts the oracle is red before start, the source pair is unchanged at `AwaitingApproval`, and both allowed paths are in the candidate.

## G4

`success_is_authorized_unlocked` requires applied candidate verification when `required_checks` is non-empty. Model verdict alone stays in review. Identity hashes regular source files, includes untracked source, ignores `target/` and other build directories, and rejects symlink escapes. Check results store outcome, exit, duration, and output digest.

Paths: `src/verified_change.rs`, `src/orchestration/store.rs`, `src/grok_build.rs` (`defer_source_apply`).

Result: skipped noop, escape, and symlink cases stay unverified. A tampered retained candidate clears `checksPassed` and `approve_work` fails. Build-output identity and secret-env unit tests passed.

## G5

Approval records the candidate digest and does not apply. Apply writes only that digest and then uses the existing success writer. Discard of a different or already-applied candidate is rejected. The disposable source is the only tree applied.

Result: after approval the source pair is still the pre-repair text. Apply then matches the repaired pair. Discard of the applied digest errors and leaves the repaired pair in place. `operator_status_apply_and_discard_bind_the_exact_digest` drives status, apply, and discard through the MCP control plane. Status after settle shows the diff and `checksPassed`. Apply before approval fails. Discard of a different digest fails. Discard of the exact digest cancels without changing the source. Apply of the exact digest after approval changes the source, and a later wrong digest does not.

## G6

Restart reopens the same orchestration store in a new host process. The cuts are:

- In-flight admission (`Dispatching`, work leased or running): recovery records `grok_dispatch_uncertain_after_restart`, leaves the source unchanged, and the safe action says not to dispatch another attempt or treat the result as verified success.
- Candidate persistence: a one-file candidate is retained, required checks did not pass, and the source is unchanged. After restart the same content digest and snapshot remain, `changeProposed` stays true, `checksPassed` stays false, and the safe action says the worker stopped without verified checks.
- Verification: required checks passed and nobody has approved. After restart `checksPassed` stays true, `humanApproved` and `applied` stay false, the source stays unchanged, and the safe action says to review the diff because approval does not apply the change.
- After approval and before apply: the work stays `AwaitingApproval`, the source stays unchanged, and the safe action says application is separate.
- After apply: the work stays `Succeeded`, the repaired source remains, and the safe action says no further application is safe.

Each reopen keeps one attempt. Repeating the original start request does not add an attempt. Cancellation of an in-flight hold, followed by the child's late completion, stays `cancelled` and does not change the source.

Issue #521 is already repaired on the base by `WorkMutationIntent` in `store.rs`. The assignment path used here goes through that mutation. Regressions on the base, re-run in spirit by the store tests present on main: `work_mutation_intent_recovers_after_decision_only_crash` and the sibling intent tests, 4 passed at freeze. This goal did not add a second decision ledger.

Result: `cancel_and_restart_do_not_dispatch_or_apply`, `restart_at_admission_approval_and_apply_names_a_safe_action`, and `restart_after_candidate_persistence_and_verification` passed. The persistence and verification restart test passed on its own, again with the workflow file (5 passed), and again inside the full locked suite.

## G7

| # | Case | Result |
| --- | --- | --- |
| 1 | Multi-file red then green | pass, workflow test |
| 2 | Failed, missing, or skipped checks | pass, noop plus unit test |
| 3 | Post-verification mutation | pass, tamper then approve fails |
| 4 | Out-of-scope and symlink | pass, escape and symlink tests plus identity unit test |
| 5 | Cancel plus late completion | pass |
| 6 | Restart at admission, candidate persistence, verification, and approval/application boundaries has truthful recovery | pass, `restart_at_admission_approval_and_apply_names_a_safe_action` and `restart_after_candidate_persistence_and_verification` |
| 7 | Duplicate start | pass, one attempt |
| 8 | Unsupported readiness | pass, zero intents |
| 9 | Exact candidate apply and discard | pass |
| 10 | No credentials in the projection | pass, `assert_secret_free` |

Bridge clippy `--locked --all-targets -- -D warnings`: exit 0 on the functional commit below. `cargo fmt --all -- --check`: exit 0 after the one formatting wrap included in that commit.

Regression: `cargo test --locked -- --test-threads=1` from `crates/codegen/grokptah-agent-bridge` exited 0. Lib tests: 653 passed. Adapter 27 passed. Managed executor non-live test 1 passed. Live test ignored, not counted as a pass. `grokptah-service` `cargo check --locked --all-targets` exited 0.

The same suite against the operator's existing `~/.grokptah` failed three `provider_qualification` tests because that authority file cannot be opened (`missing field policy_revision`). Those three tests passed on a fresh `GROKPTAH_HOME`. This branch does not modify that store. The passing suite used a fresh home.

## Identity

- Tested functional SHA: `32268e89f52776704d7a4729c2bd3581310ceeb7`
- Tested functional tree: `74727df90e187f906b1bdd912742073444978166`
- `7f9ba9dc1c23cfc3052680b7f6736638d041cfa9` (tree `905375f539ac08b1a845cc3646207a14894367a2`) added the candidate-persistence and checks-passed restart cuts. The SHA above adds settled status, apply, and discard on the desktop and MCP operator entries. Clippy, the full locked bridge suite, desktop typecheck, and desktop npm test were run on that tree.
- `9b02ecc986c77e6872ec22ae6beb6377075e9493` (tree `550d81116bc0c7bb064ac3e814fe4272836c0a48`) added the admission, approval, and apply restart cuts.
- `aed1248fbdbebca002b4bfb43d24fbc2653587e0` (tree `53d01d981e7b6822944a43a10c437c0a5e4efbc2`) added the operator control-plane entry.
- Earlier functional commit `ab2f86ea5cadd89321794a7ca50609c11ed171d9` (tree `f113106787c39bd16e7d6901a8f49d4cb70e1de2`) is the candidate-verification slice.
- The commit that updates this identity section is evidence-only. It does not change executable code. Any later executable change requires the applicable tests again.

## G8

Draft PR: https://github.com/chriscase/GrokPtah/pull/579
Remote branch: `grok/verified-change-workflow-v1`
The functional SHA above is the tested executable tree. This evidence commit is documentation only and is the published tip once `git ls-remote` matches it.

## Repair after independent review

Independent exact-source review of `32268e89f52776704d7a4729c2bd3581310ceeb7` returned HOLD / REWRITE. The green fixture suite on that SHA is not treated as a disproof. The repair is `bd4476cfd89b1e24f279d8883f01cfe4e74201a4` (tree `c1e1024a2908491f28023fe9c52a78eac93aa20c`).

| Finding | Disposition | Named regression |
| --- | --- | --- |
| P0: source application is not crash/restart/partial-failure recoverable | Repaired. Apply persists `ApplySourceIntent` before any git effect and classifies restart as not applied, already applied, or poisoned. A second-path failure rolls back. A crash after the first effect is reconciliation, not a clean no-op. Discard cleanup failure does not mark the work cancelled. | `apply_faults_before_effect_retry_once_and_a_failed_command_changes_nothing` (cuts 1, 2, 6); `second_path_failure_rolls_back_and_a_crash_is_not_a_clean_noop` (cuts 3 and 9); `restart_after_source_effect_commits_without_applying_again` (cuts 4 and 8); `restart_after_work_commit_finishes_the_idempotency_response` (cut 5); `rollback_failure_blocks_automatic_continuation` (cut 7); `discard_cleanup_failure_is_not_a_completed_discard` |
| P0: production `FileCredentialLease` does not revoke upstream authority | Repaired by removal from the operator path. `configure_managed_grok_from_operator_env` returns an error when `GROKPTAH_MANAGED_GROK_EXECUTABLE` is set and does not install a file lease. Readiness stays unavailable and dispatches nothing unless the resolver reports `revokes_upstream`. File truncation is not revocation. | `file_truncation_does_not_revoke_an_already_read_lease` |
| P1: candidate source revision is re-read from mutable HEAD | Repaired. Verification copies the launch SHA and source fingerprint from `ManagedGrokInvocation`. | `verification_stays_bound_to_the_launch_sha` |
| P1: required checks are caller-selected executables | Repaired. Public prepare/start take an opaque `check_profile_id`. The runtime home resolves the executable, digests, argv, cwd, oracle, environment, limits, and network policy. Replacement of the executable or oracle invalidates the check. Timeout kills the process group. Checks do not inherit operator `HOME`, `PATH`, or `GITHUB_TOKEN`, cannot write the source or candidate, and cannot use the network unless the profile says `qualified`. | `replaced_executable_or_oracle_invalidates_without_running`; `timed_out_check_kills_the_background_process_group`; `check_cannot_write_source_or_candidate_or_inherit_operator_env`; `forbidden_network_is_denied_and_qualified_network_can_connect` |
| P1: status revalidates before the Work is in scope | Repaired. Status authorizes the session and workspace, then revalidates under that scope. Foreign, unknown, and malformed ids do not reveal the diff or clear approval. | `foreign_status_does_not_reveal_or_mutate_the_candidate` |
| P1: readiness trusts caller platform/host and start rewrites Agent policy | Repaired. Platform is the host OS and the execution host is the process surface. A failed readiness check creates no admission. Executor budget and one-attempt authority travel in a Work-scoped envelope. The persistent Agent `managed_execution` policy is not rewritten. Retrying the same request id with a changed profile conflicts and leaves agent bytes unchanged. | `same_request_with_a_changed_profile_conflicts`; `multi_file_repair_is_red_then_green_and_apply_is_separate` (caller platform `linux` is ignored) |
| P1: candidate identity omits modes and hashes the whole tree | Repaired. Identity is the launch base SHA plus a changed-path manifest and binary patch. Each path records mode, blob, add/modify/delete/mode, symlink, and untracked. A mode-only change applies the mode and keeps the bytes. Unchanged files are not hashed. Remaining bounds are 2000 paths and 32 MiB; readiness names that limit before dispatch. | `mode_only_change_applies_exactly`; `changed_path_identity_does_not_hash_unchanged_files` |

Local validation on this functional tree, before this evidence text:

- `cargo test --locked --test verified_change_workflow -- --test-threads=1`: 15 passed.
- `cargo test --locked --lib -- timed_out_check_kills replaced_executable check_cannot_write forbidden_network`: 4 passed. The timeout test finished in the same second as the other three and found no surviving `sleep 47`.
- `cargo test --locked -- --test-threads=1` from `crates/codegen/grokptah-agent-bridge` with a fresh `GROKPTAH_HOME`: exit 0. Lib tests 660 passed. `grok_build_adapter` 27 passed. `grok_build_managed_executor` 1 passed and `live_grok_build_dogfood_runs_both_profiles_under_one_authority` ignored. `reliability_eval` 1 passed. `verified_change_workflow` 15 passed inside that suite.
- `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings` in the bridge: exit 0. The test-gateway fmt check exited 0.
- `grokptah-service`: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked -- --test-threads=1`, and `cargo check --locked --all-targets` exited 0.
- Desktop `npm run typecheck` exited 0. `npm test` exited 0 (58 files, 428 tests). `desktop/src-tauri` `cargo test --locked` exited 0 (50 lib tests passed).
- `cargo test -p xai-host-authority --locked -- --test-threads=1` from the repository root exited 0.

Hosted Desktop workflow for the repaired functional SHA `bd4476cfd89b1e24f279d8883f01cfe4e74201a4`: GitHub Actions run `35794089280` (https://github.com/chriscase/GrokPtah/actions/runs/35794089280) completed with conclusion `success`. The run event was `pull_request`, `head_sha` was that functional SHA, and the `desktop` job succeeded with no failed steps. It started `2026-09-22T22:45:55Z` and finished `2026-09-22T23:21:22Z`.

Hosted Desktop workflow for the evidence tip `2ddc65577b7716c448c10e071231846cc094a8ca`, recorded separately: GitHub Actions run `35797121302` (https://github.com/chriscase/GrokPtah/actions/runs/35797121302) also completed with conclusion `success`. Its `head_sha` was that evidence tip, not the functional SHA. It started `2026-09-22T23:22:46Z` and finished `2026-09-23T00:01:26Z`. The `desktop` job succeeded with no failed steps. That run does not replace the functional-head result above. This sentence is itself a docs-only change, so publishing it can queue another Desktop run for a newer tip. That later run is not a new functional result.

The live-provider test was not run.

## Follow-up after hosted review

Three gaps remained after `2216b8e18`:

- A check profile with network `qualified` skipped `sandbox-exec`, so the check could write the source workspace. Candidate identity also ignored file mode, so a mode-only candidate edit could still pass. Qualified checks now use the same write sandbox as offline checks, with network allowed only for that policy. After the check, source status and candidate bytes and modes are compared. Regression: `qualified_network_cannot_change_source_bytes_or_candidate_mode`.
- `grokptah-service` aborted startup when `GROKPTAH_MANAGED_GROK_EXECUTABLE` was set, because a file lease cannot revoke upstream authority. Startup now logs that error and keeps serving, with readiness unavailable and no worker dispatched. Regression: `unavailable_operator_lease_does_not_abort_service_startup`.
- Readiness described every dirty tree or fingerprint error as the 2000-path / 32 MiB ceiling, and did not measure the bound before dispatch. A dirty tree now says the worktree is dirty. An over-bound diff says the 2000-path or 32 MiB limit. Both refuse dispatch. Regressions: `dirty_source_names_the_worktree_and_does_not_dispatch`, `over_bound_source_names_the_path_ceiling_before_dispatch`, `one_byte_edit_is_inside_the_changed_path_bound`, `too_many_untracked_paths_name_the_file_bound`, `oversized_patch_names_the_byte_bound`.

Local proof for this follow-up, measured on functional SHA `872707b6006d9b9f6c2fe94a09e57d2093973fbb` (tree `450ff6e55d52800729ec1d43e4fb4cdb285d027b`) before this evidence text:

- `cargo test --locked -- --test-threads=1` in `crates/codegen/grokptah-agent-bridge` with a fresh `GROKPTAH_HOME`: exit 0. Lib tests 664 passed, including `mode_only_change_applies_exactly`, `one_byte_edit_is_inside_the_changed_path_bound`, `too_many_untracked_paths_name_the_file_bound`, `oversized_patch_names_the_byte_bound`, `work_lifecycle_reopen_is_deterministic_at_each_crash_cut`, and the other store lifecycle and crash-recovery tests. `grok_build_adapter` 27 passed. `grok_build_managed_executor` 1 passed and `live_grok_build_dogfood_runs_both_profiles_under_one_authority` ignored. `reliability_eval` 1 passed. `verified_change_workflow` 17 passed.
- `cargo test -p xai-host-authority --locked -- --test-threads=1` from the repository root: exit 0.
- Headless `grokptah-service` `cargo check --locked --all-targets`: exit 0. `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`, run while the bridge suite and authority suite were also compiling, exited 101. `disconnect_reconnect_restart_and_cursor_expiry_are_durable` failed because shutdown reported `journal writer queue is full` and retained the instance lock, so the restart could not acquire it. An isolated rerun of the same service command exited 0: lib 4 passed, `service_conformance` 11 passed, `service_smoke` 5 passed.

Hosted Desktop for functional SHA `872707b6006d9b9f6c2fe94a09e57d2093973fbb`: GitHub Actions run `35804342846` (https://github.com/chriscase/GrokPtah/actions/runs/35804342846) completed with conclusion `success`. The run event was `pull_request`, `head_sha` was that functional SHA, and the `desktop` job succeeded with no failed steps. It started `2026-09-23T00:58:29Z` and finished `2026-09-23T01:29:33Z`.

The commit that first added the suite paragraph is evidence-only SHA `5cb0ee3e42619c0cb8d1b8f17bf91bc48a7eab11` (tree `67eceedff884bbd754dabf552b8957ff4374645e`). It does not change executable code. Its SHA is not `872707b60`.

Hosted Desktop for that published evidence tip `5cb0ee3e42619c0cb8d1b8f17bf91bc48a7eab11`, recorded separately from the functional-head run: GitHub Actions run `35807355869` (https://github.com/chriscase/GrokPtah/actions/runs/35807355869) completed with conclusion `success`. The run event was `pull_request`, `head_sha` was `5cb0ee3e42619c0cb8d1b8f17bf91bc48a7eab11`, and the `desktop` job succeeded with no failed steps. It started `2026-09-23T01:41:49Z` and finished `2026-09-23T02:02:01Z`. Run `35804342846` remains the result for functional SHA `872707b60` only. It is not the result for `5cb0ee3e`.

Hosted Desktop for published tip `a1d65361fcb120e975740963cc571eedc8cffc48` (tree `f198a5e97809dde6b53258d4edef6e2d6c8439a4`): GitHub Actions run `35808819822` (https://github.com/chriscase/GrokPtah/actions/runs/35808819822) completed with conclusion `success`. The run event was `pull_request`, `head_sha` was `a1d65361fcb120e975740963cc571eedc8cffc48`, and the `desktop` job succeeded with no failed steps. It started `2026-09-23T02:03:22Z` and finished `2026-09-23T02:33:58Z`. This is the hosted result for that tip. It is not the result for `872707b60` or for `5cb0ee3e`.

The commit that adds this paragraph is a later docs-only change. Its Desktop run, if one starts, is not finished here and is not claimed.


## Durable authority seals

Independent review of published tip `7443e0b1130161460aa17bf2701129ad8ab54005` returned HOLD. The executable tree under review was still functional SHA `872707b6006d9b9f6c2fe94a09e57d2093973fbb` (tree `450ff6e55d52800729ec1d43e4fb4cdb285d027b`). This repair is functional SHA `e14ac60458bd7d29aa7562e03575f439ad494e56` (tree `91faf17b9a91520781f4ab65d4b2f4664c76e042`). It is not an independent acceptance of the frozen goal.

| Finding | Disposition | Named regression |
| --- | --- | --- |
| P0: missing or corrupt check authority still executes and can pass | Repaired. A verified-change Work seals a schema-versioned check authority into `CandidateVerification`. Missing, unreadable, malformed, stale, or mismatched authority launches no check, cannot reach `AwaitingApproval`, and cannot authorize success. `authorizes_applied_success` requires the profile id, profile revision, and authority digest. | `missing_check_authority_runs_no_check_and_cannot_verify`; `malformed_check_authority_runs_no_check_and_cannot_verify`; `replaced_executable_or_oracle_invalidates_without_execution`; `replaced_executable_or_oracle_invalidates_without_running`; `tampered_profile_revision_or_network_policy_cannot_verify` |
| P0: patch, manifest, and final fingerprint are not sealed to the approved digest | Repaired. Finalization stores a `CandidateApplyBundle` digest on `CandidateVerification`. Approval, revalidation, apply, and recovery reread the artifacts, recompute the digest, derive `finalFingerprint` from base plus patch, and check patch SHA-256, manifest digest, and materialized bytes and modes. | `tampered_patch_cannot_apply`; `tampered_final_fingerprint_cannot_apply`; `tampered_manifest_path_mode_or_blob_cannot_apply`; `unchanged_materialized_tree_does_not_hide_a_replaced_patch`; `candidate_bundle_tamper_clears_approval_and_changes_no_source` |
| P1: discard ignores a pending apply intent after source effects | Repaired. Discard inspects any `ApplySourceIntent` under the store lock before it removes artifacts. Already applied finishes success, completes the pending receipt, and refuses discard. Not applied retires the intent, then discards. Poisoned evidence is retained and discard is refused. | `discard_after_source_effect_before_work_commit_finishes_apply`; `discard_after_intent_before_effect_retires_intent_then_cancels`; `discard_refuses_poisoned_apply`; `concurrent_apply_and_discard_converge_to_one_truthful_result` |
| P1: apply-intent fields are not validated on recovery | Repaired. `ApplySourceIntent::validate_against` runs before source classification or receipt completion. A mismatched schema, Work, revision, candidate, bundle, attempt, approval, patch, derived fingerprint, principal, policy revision, or idempotency receipt is quarantine, not success. The recorded `finalFingerprint` is not success authority. | `tampered_apply_intent_final_fingerprint_cannot_fabricate_success`; `tampered_apply_intent_patch_or_candidate_digest_cannot_recover`; `stale_apply_intent_cannot_finish_a_newer_work_revision`; `foreign_apply_intent_cannot_complete_another_receipt`; `unsupported_apply_intent_schema_fails_closed` |
| P1: required checks run unsandboxed when `sandbox-exec` is absent | Repaired. Readiness and the sealed authority require the macOS `sandbox-exec` backend. A missing backend or a failed sandbox launch runs no check process and does not fall back to the executable. | `missing_check_sandbox_is_not_ready`; `missing_check_sandbox_runs_no_process`; `sandbox_launch_failure_cannot_fall_back_unsandboxed` |
| P1: the execution envelope is an untyped mutable sidecar | Repaired. `VerifiedExecutionEnvelopeV1` replaces arbitrary JSON parsing. Its canonical digest is bound into the Work authorization decision. A missing, malformed, tampered, foreign, or mismatched envelope does not dispatch. | `injected_execution_envelope_cannot_enable_grok_for_unrelated_work`; `tampered_execution_budget_or_profile_cannot_dispatch`; `missing_execution_envelope_is_ineligible_without_side_effects` |

Local validation on functional SHA `e14ac60458bd7d29aa7562e03575f439ad494e56`, before this evidence text:

- `cargo test --locked -- --test-threads=1` in `crates/codegen/grokptah-agent-bridge` with a fresh `GROKPTAH_HOME`: exit 0. Lib tests 670 passed, including the run-promotion tests and `work_lifecycle_reopen_is_deterministic_at_each_crash_cut`. `grok_build_adapter` 27 passed. `grok_build_managed_executor` 1 passed and `live_grok_build_dogfood_runs_both_profiles_under_one_authority` ignored. `reliability_eval` 1 passed. `verified_change_workflow` 35 passed.
- `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings` in the bridge: exit 0.
- `cargo test -p xai-host-authority --locked -- --test-threads=1` from the repository root: exit 0.
- Headless `grokptah-service` on a fresh `GROKPTAH_HOME`, not overlapping the bridge suite: `cargo test --locked -- --test-threads=1` exit 0 (lib 4, `service_conformance` 11, `service_smoke` 5) and `cargo check --locked --all-targets` exit 0.
- Desktop `npm run typecheck` exit 0. `npm test` exit 0 (58 files, 428 tests). `desktop/src-tauri` `cargo test --locked` exit 0 (50 lib tests).

Hosted Desktop for functional SHA `e14ac60458bd7d29aa7562e03575f439ad494e56`: GitHub Actions run `35898331687` (https://github.com/chriscase/GrokPtah/actions/runs/35898331687) completed with conclusion `success`. The run event was `pull_request`, `head_sha` was that functional SHA, and the `desktop` job succeeded with no failed steps. It started `2026-09-23T17:50:12Z` and the job finished `2026-09-23T18:24:56Z`.

The commit that adds this section is documentation only. Its SHA is not `e14ac60458bd7d29aa7562e03575f439ad494e56`. A Desktop run for that docs tip, if one starts, is not the functional result above.

The live-provider test was not run. No production revocable xAI lease was implemented. `FileCredentialLease` stays test-only. `HostLeaseAuthority` stays an in-process fake.

## Publication record for the authority seals

- Prior functional SHA: `872707b6006d9b9f6c2fe94a09e57d2093973fbb`
- Prior functional tree: `450ff6e55d52800729ec1d43e4fb4cdb285d027b`
- Repaired functional SHA: `e14ac60458bd7d29aa7562e03575f439ad494e56`
- Repaired functional tree: `91faf17b9a91520781f4ab65d4b2f4664c76e042`
- Evidence tip that first recorded run `35898331687`: `fee6a5c4433d5907b89a1c1b223ec662c354eb1b`
- Evidence tip tree: `03428e51ba243bdf1bb9f380aa9530b5b6715d95`
- When `fee6a5c44` was pushed, `git ls-remote origin refs/heads/grok/verified-change-workflow-v1` equaled `fee6a5c4433d5907b89a1c1b223ec662c354eb1b`, and `origin/main` stayed `0dbe51c8aa94e3f26543be7a90e0085029487de7`.
- Executable changes after tested revision `e14ac60458bd7d29aa7562e03575f439ad494e56`: none. The diff through `fee6a5c44` is only `docs/goals/verified-change-workflow-v1-evidence.md`.

| Check | Result on `e14ac6045` |
| --- | --- |
| C1 sealed check authority | pass, named regressions in the table above |
| C2 candidate apply bundle | pass, named regressions in the table above |
| C3 apply-intent validation | pass, named regressions in the table above |
| C4 discard serialized with apply recovery | pass, named regressions in the table above |
| C5 check confinement fails closed | pass, named regressions in the table above |
| C6 typed execution envelope | pass, named regressions in the table above |
| C7 local suites and hosted Desktop | pass. Local suites listed above. Hosted run `35898331687` has `head_sha` `e14ac60458bd7d29aa7562e03575f439ad494e56` |

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

Desktop run `35902522879` has `head_sha` `fee6a5c4433d5907b89a1c1b223ec662c354eb1b`. It is not a result for `e14ac60458bd7d29aa7562e03575f439ad494e56`. This paragraph is a later docs-only commit. A Desktop run whose `head_sha` is not `e14ac60458bd7d29aa7562e03575f439ad494e56` is not claimed.

## Invocation and approval-bundle seals

A follow-up review of functional SHA `e14ac60458bd7d29aa7562e03575f439ad494e56` (tree `91faf17b9a91520781f4ab65d4b2f4664c76e042`) found two remaining holes. This repair is functional SHA `2240ed985dc3f7ade21cfd1ddfaa9f366df21486` (tree `eb73caffb771928dba9f59f75cb8a4087e4f4671`). It is not an independent acceptance of the frozen goal.

| Finding | Disposition | Named regression |
| --- | --- | --- |
| Check authority digest omitted the executable path, oracle root, argv, cwd, env, timeout, process limit, write policy, and source root, so a rewritten check spec still launched | Repaired. Those fields are part of the canonical authority digest. A spec that does not match the sealed invocation launches no process. | `rewritten_check_argv_cwd_env_or_timeout_runs_no_process` |
| Approval stored only the manifest digest. Replacing `promotion.patch`, aligning `finalFingerprint`, and rewriting `applyBundleDigest` applied the unapproved patch. Check outcomes and materialized bytes were not bundle inputs. | Repaired. Approval stores the recomputed apply-bundle digest. Apply and success authorization require that pin. The bundle hashes check outcomes and the on-disk candidate bytes and modes. | `resealed_patch_after_approval_cannot_apply` |

Local validation on `2240ed985dc3f7ade21cfd1ddfaa9f366df21486`, before this evidence text:

- `cargo test --locked -- --test-threads=1` in `crates/codegen/grokptah-agent-bridge` with a fresh `GROKPTAH_HOME`: exit 0. The suite includes lib tests, `verified_change_workflow` (36 passed, including the two regressions above), adapter 27, managed-executor non-live with the live test ignored, `reliability_eval`, and the run-promotion and lifecycle recovery tests.
- Bridge `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings`: exit 0.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0.
- Isolated headless service `cargo test --locked -- --test-threads=1` and `cargo check --locked --all-targets`: exit 0.
- Desktop `npm run typecheck`, `npm test`, and `desktop/src-tauri` `cargo test --locked`: exit 0.

Hosted Desktop for functional SHA `2240ed985dc3f7ade21cfd1ddfaa9f366df21486`: GitHub Actions run `35907264876` (https://github.com/chriscase/GrokPtah/actions/runs/35907264876) completed with conclusion `success`. The event was `pull_request`, `head_sha` was that functional SHA, and the `desktop` job succeeded with no failed steps. The run started `2026-09-23T19:08:05Z` and the job finished `2026-09-23T19:51:43Z`.

The commit that adds this section is documentation only. Its SHA is not `2240ed985dc3f7ade21cfd1ddfaa9f366df21486`. A Desktop run for that docs tip is not this result.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Exact candidate, crash classification, and process-tree authority

Prior functional SHA: `2240ed985dc3f7ade21cfd1ddfaa9f366df21486` (tree `eb73caffb771928dba9f59f75cb8a4087e4f4671`). This repair is functional SHA `c33593bede2263b96849ec7e98f9beb92352be28` (tree `41063bc923618069159c3bd856c1b54021447fb2`). It is not an independent acceptance of the frozen goal.

| Gap | Disposition | Named regression |
| --- | --- | --- |
| One stable candidate | Repaired. Normal Grok Build completion proves the worker process group is gone before capture. One capture records HEAD, ref, status, the canonical patch, the changed-path manifest, and file bytes, modes, deletions, and symlinks; materializes that tree; recaptures; and refuses a mismatch. Applying the retained patch to a fresh detached checkout at the base SHA reproduces the checked tree. Patch paths equal the manifest, the adapter changed paths, and Work `allowed_files`. | `normal_exit_with_surviving_mutator_cannot_retain_a_candidate`; `preverification_patch_tree_mismatch_cannot_verify`; `applied_patch_must_reproduce_the_checked_materialized_tree`; `patch_paths_must_equal_manifest_and_allowed_scope` |
| Apply recovery classification | Repaired. Replay, store-open recovery, discard, receipt completion, and reconciliation classify the sealed manifest against the complete worktree, including untracked additions, without depending on intent-to-add. Before-identity with no foreign changes is NotApplied. After-identity with no foreign changes is AlreadyApplied. Mixed, unknown, or foreign changes are Poisoned. | `crash_after_first_untracked_add_is_not_classified_not_applied`; `crash_after_all_new_files_before_index_update_recovers_applied`; `discard_never_leaves_an_untracked_candidate_file`; `foreign_untracked_file_during_apply_requires_reconciliation` |
| Check process tree | Repaired. One wall-clock deadline covers the leader, descendants, stdout, stderr, and output-directory growth. A leader that exits while a descendant holds a pipe does not block past that deadline. Every outcome proves the process group is gone or returns an unproved-termination failure. Each invocation uses a distinct mode-0700 output directory whose byte limit is enforced during the run. `processLimit` is removed from the authority contract and from public claims; a sealed limit of 1 cannot mean one OS process under `sandbox-exec` plus the check script. | `successful_parent_with_background_pipe_holder_obeys_timeout`; `normal_check_exit_requires_process_group_quiescence`; `concurrent_checks_have_distinct_private_output_directories`; `output_directory_limit_terminates_the_check_tree` |
| Check authority bound to the work decision | Repaired. The complete check authority is sealed before `VerifiedExecutionEnvelopeV1` is built, and that digest is inside the envelope bound to the authorization decision. Dispatch and finalization reject an authority whose digest or work, session, or workspace identity differs. Candidate verification and the apply bundle carry the original digest. | `resealed_check_authority_cannot_upgrade_network_or_resource_policy`; `foreign_check_authority_cannot_be_copied_to_another_work`; `authority_identity_mismatch_runs_no_check` |
| Accepted repairs | Kept. A rewritten check argv, cwd, env, or timeout still launches no process. Approval still pins the apply-bundle digest. | `rewritten_check_argv_cwd_env_or_timeout_runs_no_process`; `resealed_patch_after_approval_cannot_apply` |

Local validation on `c33593bede2263b96849ec7e98f9beb92352be28`, before this evidence text:

- `cargo test --locked -- --test-threads=1` in `crates/codegen/grokptah-agent-bridge` with a fresh `GROKPTAH_HOME`: exit 0. `verified_change_workflow` passed 47 tests. The live managed-executor test stayed ignored.
- Bridge `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings`: exit 0.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0.
- Isolated headless service `cargo test --locked -- --test-threads=1` and `cargo check --locked --all-targets`: exit 0.
- Desktop `npm run typecheck`, `npm test`, and `desktop/src-tauri` `cargo test --locked`: exit 0.

Hosted Desktop for functional SHA `c33593bede2263b96849ec7e98f9beb92352be28`: GitHub Actions run `35937962980` (https://github.com/chriscase/GrokPtah/actions/runs/35937962980) completed with conclusion `success`. The event was `pull_request`, `head_sha` was that functional SHA, and the `desktop` job succeeded with no failed steps. The run started `2026-09-24T00:20:21Z` and the job finished `2026-09-24T00:48:01Z`.

The commit that adds this section is documentation only. Its SHA is not `c33593bede2263b96849ec7e98f9beb92352be28`. A Desktop run for that docs tip is not this result.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Retained-byte evidence and full object ids

A review of functional SHA `c33593bede2263b96849ec7e98f9beb92352be28` (tree `41063bc923618069159c3bd856c1b54021447fb2`) found two remaining holes. This repair is functional SHA `cf9d03a1a114e79805797b1df8429dd9182dd445` (tree `1e6b830b0726545f5654b330b0cd937034ca5d8f`). It is not an independent acceptance of the frozen goal.

| Finding | Disposition | Named regression |
| --- | --- | --- |
| Adapter mutation evidence was an independent status and file hash taken before retention. Only path sets were compared, and that earlier digest was what the work stored. | Repaired. Changed paths and the evidence digest are derived from the retained patch, manifest, and materialized bytes. A checkout whose bytes differ is refused. | `retained_tree_byte_mismatch_cannot_bind_adapter_evidence`. `applied_patch_must_reproduce_the_checked_materialized_tree` also checks that the stored digest equals `retained_candidate_diff_digest`. |
| Manifest identity accepted a `git diff --raw` abbreviation as a prefix of `git hash-object`. A different file sharing that prefix could be classified AlreadyApplied. | Repaired. Capture stores full 40-character object ids. Identity requires exact equality. | `abbreviated_blob_prefix_is_not_already_applied` |

Local validation on `cf9d03a1a114e79805797b1df8429dd9182dd445`, before this evidence text:

- Locked bridge suite on a fresh `GROKPTAH_HOME`: exit 0. The live managed-executor test stayed ignored.
- Bridge fmt and strict all-target Clippy: exit 0.
- Host-authority suite: exit 0.
- Isolated headless service tests and `cargo check --locked --all-targets`: exit 0.
- Desktop typecheck, npm test, and Tauri tests: exit 0.

Hosted Desktop for functional SHA `cf9d03a1a114e79805797b1df8429dd9182dd445`: GitHub Actions run `35942601248` attempt 3 (https://github.com/chriscase/GrokPtah/actions/runs/35942601248) completed with conclusion `success`. The event was `pull_request`, `head_sha` was that functional SHA, and the `desktop` job succeeded with no failed steps. The successful job started `2026-09-24T02:08:30Z` and finished `2026-09-24T02:36:34Z`. Attempts 1 and 2 of the same run failed in unrelated journal-lock and continuity-probe tests and are not this result.

The commit that adds this section is documentation only. A Desktop run for that docs tip is not this result.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Publication record for the exact-candidate repair

Prior functional SHA: `2240ed985dc3f7ade21cfd1ddfaa9f366df21486`
Prior functional tree: `eb73caffb771928dba9f59f75cb8a4087e4f4671`

Repaired functional SHA: `cf9d03a1a114e79805797b1df8429dd9182dd445`
Repaired functional tree: `1e6b830b0726545f5654b330b0cd937034ca5d8f`

The commands below ran on the working tree committed as `cf9d03a1a114e79805797b1df8429dd9182dd445` (tree `1e6b830b0726545f5654b330b0cd937034ca5d8f`), before that commit at `2026-09-23T20:21:59-05:00`. No executable diff exists after that commit. Each named regression below was `ok`. The live managed-executor test stayed ignored. This is not an independent acceptance of the frozen goal.

- Bridge `cargo fmt --all -- --check`: exit 0 (`FMT:0`).
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0 (`CLIPPY:0`).
- Bridge `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`: exit 0 (`SUITE:0`).
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0 (`AUTH:0`).
- Isolated service `cargo test --locked -- --test-threads=1`: exit 0 (`SERVICE:0`).
- Isolated service `cargo check --locked --all-targets`: exit 0 (`CHECK:0`).
- Desktop `npm run typecheck`: exit 0 (`TC:0`).
- Desktop `npm test`: exit 0 (`NPM:0`).
- `desktop/src-tauri` `cargo test --locked`: exit 0 (`DESKLIB:0`).

| Gap | Result on `cf9d03a1a` / `1e6b830b` | Named regression |
| --- | --- | --- |
| D1 one candidate | One capture records HEAD, ref, status, patch, manifest, bytes, modes, deletions, and symlinks, then recaptures and refuses a mismatch. A surviving process-group member blocks retention. The retained patch reproduces the checked tree. Patch paths equal the manifest, adapter paths, and `allowed_files`. Adapter evidence is that retained patch and materialized tree; differing checkout bytes are refused. | `normal_exit_with_surviving_mutator_cannot_retain_a_candidate`; `preverification_patch_tree_mismatch_cannot_verify`; `applied_patch_must_reproduce_the_checked_materialized_tree`; `patch_paths_must_equal_manifest_and_allowed_scope`; `retained_tree_byte_mismatch_cannot_bind_adapter_evidence` |
| D2 classification | Replay, store-open recovery, discard, receipt completion, and reconciliation use the sealed manifest and the full worktree, including untracked files. Full object ids are required; a short blob prefix is not AlreadyApplied. | `crash_after_first_untracked_add_is_not_classified_not_applied`; `crash_after_all_new_files_before_index_update_recovers_applied`; `discard_never_leaves_an_untracked_candidate_file`; `foreign_untracked_file_during_apply_requires_reconciliation`; `abbreviated_blob_prefix_is_not_already_applied` |
| D3 supervision | One deadline covers the leader, descendants, pipes, and output-directory growth. Each check gets a distinct mode-0700 directory with a byte limit. After every outcome the process group is proved gone or the check fails closed. `processLimit` is removed from the authority contract. | `successful_parent_with_background_pipe_holder_obeys_timeout`; `normal_check_exit_requires_process_group_quiescence`; `concurrent_checks_have_distinct_private_output_directories`; `output_directory_limit_terminates_the_check_tree` |
| D4 decision bind | The check authority is sealed before the envelope. Its digest is inside the envelope bound to the authorization decision. Dispatch and finalization reject a different digest or identity. Verification and the apply bundle keep the original digest. | `resealed_check_authority_cannot_upgrade_network_or_resource_policy`; `foreign_check_authority_cannot_be_copied_to_another_work`; `authority_identity_mismatch_runs_no_check` |
| D5 validation | Bridge suite, fmt, and strict Clippy passed on the repaired functional tree, as did host-authority, isolated service tests and check, desktop typecheck, npm tests, and Tauri tests. Hosted Desktop success for this functional SHA is only run `35942601248` attempt 3. | `rewritten_check_argv_cwd_env_or_timeout_runs_no_process`; `resealed_patch_after_approval_cannot_apply` |

Hosted Desktop for repaired functional SHA `cf9d03a1a114e79805797b1df8429dd9182dd445` only: GitHub Actions run `35942601248` attempt 3 (https://github.com/chriscase/GrokPtah/actions/runs/35942601248) concluded `success`. The event was `pull_request`. `head_sha` was `cf9d03a1a114e79805797b1df8429dd9182dd445`. The `desktop` job had no failed steps (`2026-09-24T02:08:30Z` to `2026-09-24T02:36:34Z`).

The commit that adds this section is documentation only. Its tree is not `1e6b830b0726545f5654b330b0cd937034ca5d8f`. A Desktop run for that docs tip is not the result above.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Source-state recovery

Prior functional SHA: `cf9d03a1a114e79805797b1df8429dd9182dd445` (tree `1e6b830b0726545f5654b330b0cd937034ca5d8f`). This repair is functional SHA `642dee819d4d3ebb895703a37f1c27a158bdb6b9` (tree `46e6c299c857bd2097f742d1aea8a5a48210a1c5`). It is not an independent acceptance of the frozen goal.

Discard no longer removes every manifest addition after a poisoned classification. Each path is Before, After, Absent, Foreign, or Unknown. An add is Before only when absent, After only when the sealed after bytes, type, and mode match, and Foreign when anything else is at that path. All Before with the sealed base is NotApplied. All After with that base is AlreadyApplied. A mixture of Before and exact After may remove only exact candidate additions. Any Foreign or Unknown path requires reconciliation and performs no cleanup. Pre-application directory identity keeps a pre-existing directory and removes a directory created only by a partial candidate. NotApplied and AlreadyApplied require the current HEAD to equal the sealed base SHA, a clean index, and every worktree change accounted for by the manifest. A staged change on a manifest path is reconciliation. Applying a candidate does not intent-to-add the operator index. Symlink identity is the Git blob of the exact link target. A path is not accepted as that symlink merely because its mode is `120000`.

Local validation on the working tree committed as `642dee819d4d3ebb895703a37f1c27a158bdb6b9` (tree `46e6c299c857bd2097f742d1aea8a5a48210a1c5`), before that commit at `2026-09-24T10:29:43-05:00`. No executable diff exists after that commit. Each named regression below was `ok`. The live managed-executor test stayed ignored.

- Bridge `cargo fmt --all -- --check`: exit 0 (`FMT:0`).
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0 (`CLIPPY:0`).
- Bridge `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`: exit 0 (`SUITE:0`). `verified_change_workflow` passed 57 tests. `live_grok_build_dogfood_runs_both_profiles_under_one_authority` stayed ignored.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0 (`AUTH:0`).
- Isolated service `cargo test --locked -- --test-threads=1`: exit 0 (`SERVICE:0`).
- Isolated service `cargo check --locked --all-targets`: exit 0 (`CHECK:0`).
- Desktop `npm run typecheck`: exit 0 (`TC:0`).
- Desktop `npm test`: exit 0 (`NPM:0`, 58 files, 428 tests).
- `desktop/src-tauri` `cargo test --locked`: exit 0 (`DESKLIB:0`, 50 lib tests).

| Gap | Result on `642dee819` / `46e6c299` | Named regression |
| --- | --- | --- |
| E1 foreign same-path file | Discard does not delete a foreign file or symlink at an add path. Exact After additions can be removed. Cleanup keeps a pre-existing empty directory and removes a directory created only by the partial candidate. | `foreign_file_at_candidate_add_path_is_preserved_and_discard_refuses`; `foreign_symlink_at_candidate_add_path_is_preserved`; `exact_partial_candidate_addition_can_be_removed`; `partial_add_cleanup_does_not_remove_a_preexisting_empty_directory`; `partial_add_cleanup_does_not_leave_a_candidate_created_directory` |
| E2 base head and index | A changed HEAD is not recovered as success, including when the candidate bytes were committed on that new HEAD. A staged foreign change on a manifest path blocks apply and recovery. A successful add leaves the operator index unchanged. | `changed_head_after_source_effect_cannot_recover_success`; `changed_head_with_candidate_bytes_and_foreign_commit_is_poisoned`; `staged_foreign_change_on_manifest_path_blocks_apply`; `staged_foreign_change_on_manifest_path_blocks_recovery`; `successful_added_file_application_leaves_index_unchanged` |
| E3 symlink identity | A different link target is Foreign, not AlreadyApplied. The retained and materialized target matches the Git blob of the link text. An escaping symlink is still rejected. | `different_symlink_target_is_foreign_not_already_applied`; `symlink_target_change_is_exactly_verified_or_explicitly_refused`; `symlink_escape_remains_rejected` |
| E4 validation | Bridge suite, fmt, and strict Clippy passed, as did host-authority, isolated service tests and check, desktop typecheck, npm tests, and Tauri tests. Accepted D1–D5 repairs stayed green. Hosted Desktop success for this functional SHA is only run `36020642386` attempt 1. | `rewritten_check_argv_cwd_env_or_timeout_runs_no_process`; `resealed_patch_after_approval_cannot_apply` |

Hosted Desktop for repaired functional SHA `642dee819d4d3ebb895703a37f1c27a158bdb6b9` only: GitHub Actions run `36020642386` attempt 1 (https://github.com/chriscase/GrokPtah/actions/runs/36020642386) concluded `success`. The event was `pull_request`. `head_sha` was `642dee819d4d3ebb895703a37f1c27a158bdb6b9`. The `desktop` job had no failed steps (`2026-09-24T15:30:15Z` to `2026-09-24T16:05:02Z`).

The commit that adds this section is documentation only. Its tree is not `46e6c299c857bd2097f742d1aea8a5a48210a1c5`. A Desktop run for that docs tip is not the result above.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Symlink candidate identity and foreign directories

A review of functional SHA `642dee819d4d3ebb895703a37f1c27a158bdb6b9` (tree `46e6c299c857bd2097f742d1aea8a5a48210a1c5`) found two remaining holes. Retain, patch reproduction, the retained-candidate digest, and the apply bundle read a symlink by following it. A directory at an add path whose sealed mode is `100755` made classification return an error instead of Foreign. This repair is functional SHA `2b987192a22f921cef4fd0f0eb3774508c0bbe8c` (tree `43776b3f57c170d511dfcf9788218c6fe5a63b40`). It is not an independent acceptance of the frozen goal.

Symlink identity on the candidate path is the link target text, the same bytes Git hashes for a `120000` blob. Those bytes are what retain stores, what patch reproduction compares, what the retained digest and checkout bind use, and what the apply bundle checks. The link is not followed. A path that is neither a regular file nor a symlink is Foreign before any object hash, so store-open recovery reconciles and performs no cleanup.

Local validation on the working tree committed as `2b987192a22f921cef4fd0f0eb3774508c0bbe8c` (tree `43776b3f57c170d511dfcf9788218c6fe5a63b40`), before that commit at `2026-09-24T11:40:32-05:00`. No executable diff exists after that commit. Each named regression below was `ok`. The live managed-executor test stayed ignored.

- Bridge `cargo fmt --all -- --check`: exit 0 (`FMT:0`).
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0 (`CLIPPY:0`).
- Bridge `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`: exit 0 (`SUITE:0`). `live_grok_build_dogfood_runs_both_profiles_under_one_authority` stayed ignored.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0 (`AUTH:0`).
- Isolated service `cargo test --locked -- --test-threads=1`: exit 0 (`SERVICE:0`). An earlier attempt run beside other suites failed `disconnect_reconnect_restart_and_cursor_expiry_are_durable`; the isolated rerun passed and is this result.
- Isolated service `cargo check --locked --all-targets`: exit 0 (`CHECK:0`).
- Desktop `npm run typecheck`: exit 0 (`TC:0`).
- Desktop `npm test`: exit 0 (`NPM:0`, 58 files, 428 tests).
- `desktop/src-tauri` `cargo test --locked`: exit 0 (`DESKLIB:0`, 50 lib tests).

| Gap | Result on `2b987192a` / `43776b3f` | Named regression |
| --- | --- | --- |
| Symlink candidate path | A dangling link target is retained, reproduced, bound, and included in the apply bundle as the Git blob of the link text. An escaping symlink stays rejected. A different target stays Foreign. | `symlink_target_change_is_exactly_verified_or_explicitly_refused`; `different_symlink_target_is_foreign_not_already_applied`; `symlink_escape_remains_rejected` |
| Directory at an executable add | Classification and rollback return reconciliation, not an error, and the directory remains. | `executable_add_replaced_by_a_directory_is_foreign_not_an_error` |
| Accepted repairs | Retained-candidate mismatch still refuses the bind. The check-authority and approval-bundle repairs stayed green inside the locked suite. | `retained_tree_byte_mismatch_cannot_bind_adapter_evidence`; `rewritten_check_argv_cwd_env_or_timeout_runs_no_process`; `resealed_patch_after_approval_cannot_apply` |

Hosted Desktop for repaired functional SHA `2b987192a22f921cef4fd0f0eb3774508c0bbe8c` only: GitHub Actions run `36029026678` attempt 1 (https://github.com/chriscase/GrokPtah/actions/runs/36029026678) concluded `success`. The event was `pull_request`. `head_sha` was `2b987192a22f921cef4fd0f0eb3774508c0bbe8c`. The `desktop` job had no failed steps (`2026-09-24T16:40:51Z` to `2026-09-24T17:15:20Z`).

The commit that adds this section is documentation only. Its tree is not `43776b3f57c170d511dfcf9788218c6fe5a63b40`. A Desktop run for that docs tip is not the result above.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Stale cleanup and symlink containment

Prior functional SHA: `2b987192a22f921cef4fd0f0eb3774508c0bbe8c` (tree `43776b3f57c170d511dfcf9788218c6fe5a63b40`). This repair is functional SHA `112b0032d306015c412d2ebf8931fba1ca677e55` (tree `a8e9238438bd4e7cc1e111963dd97d59cab8acea`). It is not an independent acceptance of the frozen goal.

Stale cleanup runs only after schema and bounds, Work identity, candidate and apply-bundle digests, approval and attempt, patch, manifest, materialized tree, final fingerprint, exact base SHA, allowed-file scope, idempotency receipt and payload, and the source-cleanup plan digest have been checked. The stale result is a typed verdict. Directory provenance is `grokptah-source-cleanup-plan-v1`, stored on the apply intent and the apply receipt. Rollback refuses a manifest path outside the Work allowed files. An all-Before file set still removes directories created only for candidate additions. Cleanup has six fault cuts. Symlink containment resolves each existing ancestor, rejects absolute targets and targets that enter `.git` or `.grokptah`, and allows a dangling target only when its longest existing ancestor stays inside the workspace. A regular-to-symlink or symlink-to-regular change is refused before verification.

Local validation on the working tree committed as `112b0032d306015c412d2ebf8931fba1ca677e55` (tree `a8e9238438bd4e7cc1e111963dd97d59cab8acea`), before that commit at `2026-09-24T14:05:39-05:00`. No executable diff exists after that commit. Each named regression below was `ok`. The live managed-executor test stayed ignored.

- Bridge `cargo fmt --all -- --check`: exit 0 (`FMT:0`).
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0 (`CLIPPY:0`).
- Bridge `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`: exit 0 (`SUITE:0`). `verified_change_workflow` passed 66 tests. `live_grok_build_dogfood_runs_both_profiles_under_one_authority` stayed ignored.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0 (`AUTH:0`).
- Isolated service `cargo test --locked -- --test-threads=1`: exit 0 (`SERVICE:0`).
- Isolated service `cargo check --locked --all-targets`: exit 0 (`CHECK:0`).
- Desktop `npm run typecheck`: exit 0 (`TC:0`).
- Desktop `npm test`: exit 0 (`NPM:0`, 58 files, 428 tests).
- `desktop/src-tauri` `cargo test --locked`: exit 0 (`DESKLIB:0`).

| Gap | Result on `112b0032d` / `a8e92384` | Named regression |
| --- | --- | --- |
| F1 stale cleanup | A tampered manifest, an expanded path, a tampered directory-provenance seal, or an unapproved bundle performs no cleanup. An ordinary stale partial addition is still discarded. | `stale_intent_with_tampered_manifest_performs_zero_cleanup`; `stale_intent_cannot_expand_cleanup_beyond_allowed_files`; `tampered_preexisting_directory_provenance_is_quarantined`; `stale_cleanup_requires_the_approved_apply_bundle`; `ordinary_stale_partial_addition_can_still_be_safely_discarded` |
| F2 directory durability | A crash after file removal and before directory removal is recovered. All-Before paths still remove candidate-created directories. A preexisting empty directory survives every cleanup cut. The discard receipt completes only after restoration. | `crash_after_file_cleanup_before_directory_cleanup_recovers`; `all_before_paths_still_reconcile_candidate_created_directories`; `preexisting_empty_directory_survives_every_cleanup_cut`; `discard_receipt_completes_only_after_full_source_restoration` |
| F3 symlink containment | A dangling target through an escaping ancestor, a `..` after a symlink, and a target into `.git` or `.grokptah` are rejected. A safe dangling internal target is retained exactly. | `dangling_target_through_escaping_symlink_ancestor_is_rejected`; `parent_component_after_symlink_cannot_escape`; `symlink_target_into_git_metadata_is_rejected`; `safe_dangling_internal_target_is_retained_exactly` |
| F4 type transitions | A regular/symlink transition is refused before verification. An unchanged source is NotApplied and does not require reconciliation. | `regular_to_symlink_applies_exactly_or_is_refused_before_review`; `symlink_to_regular_applies_exactly_or_is_refused_before_review`; `type_transition_with_unchanged_source_never_sets_reconciliation_required` |
| Accepted repairs | Retained symlink identity, foreign-path preservation, changed HEAD, an unchanged operator index, patch reproduction, check authority, and the approval-bound bundle stayed green. | `symlink_target_change_is_exactly_verified_or_explicitly_refused`; `foreign_file_at_candidate_add_path_is_preserved_and_discard_refuses`; `changed_head_after_source_effect_cannot_recover_success`; `successful_added_file_application_leaves_index_unchanged`; `applied_patch_must_reproduce_the_checked_materialized_tree`; `rewritten_check_argv_cwd_env_or_timeout_runs_no_process`; `resealed_patch_after_approval_cannot_apply` |

Hosted Desktop for repaired functional SHA `112b0032d306015c412d2ebf8931fba1ca677e55` only: GitHub Actions run `36045789618` attempt 1 (https://github.com/chriscase/GrokPtah/actions/runs/36045789618) concluded `success`. The event was `pull_request`. `head_sha` was `112b0032d306015c412d2ebf8931fba1ca677e55`. The `desktop` job had no failed steps (`2026-09-24T19:15:08Z` to `2026-09-24T19:51:11Z`).

The commit that adds this section is documentation only. Its tree is not `a8e9238438bd4e7cc1e111963dd97d59cab8acea`. A Desktop run for that docs tip is not the result above.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Case-variant metadata symlink targets

Prior functional SHA: `112b0032d306015c412d2ebf8931fba1ca677e55` (tree `a8e9238438bd4e7cc1e111963dd97d59cab8acea`). This repair is functional SHA `d7306c1e7d346d1ee9ed6f36e06e457d5a65c6c6` (tree `a121de35fe498bc9fb147f53ccfa5f034949d95f`). It is not an independent acceptance of the frozen goal.

Symlink containment compares `.git` and `.grokptah` with the same ASCII case-insensitive rule as relative-path validation. A dangling target `.Grokptah/secret` or `.GIT/config` is rejected at capture. The exact lowercase forms stay rejected, and a safe dangling internal target stays retained.

Local validation on the working tree committed as `d7306c1e7d346d1ee9ed6f36e06e457d5a65c6c6` (tree `a121de35fe498bc9fb147f53ccfa5f034949d95f`), before that commit at `2026-09-24T15:36:06-05:00`. No executable diff exists after that commit. The named regression below was `ok`. The live managed-executor test stayed ignored.

- Bridge `cargo fmt --all -- --check`: exit 0 (`FMT:0`).
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0 (`CLIPPY:0`).
- Bridge `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`: exit 0 (`SUITE:0`). `live_grok_build_dogfood_runs_both_profiles_under_one_authority` stayed ignored.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0 (`AUTH:0`).
- Isolated service `cargo test --locked -- --test-threads=1`: exit 0 (`SERVICE:0`).
- Isolated service `cargo check --locked --all-targets`: exit 0 (`CHECK:0`).
- Desktop `npm run typecheck`: exit 0 (`TC:0`).
- Desktop `npm test`: exit 0 (`NPM:0`, 58 files, 428 tests).
- `desktop/src-tauri` `cargo test --locked`: exit 0 (`DESKLIB:0`).

| Gap | Result on `d7306c1e7` / `a121de35` | Named regression |
| --- | --- | --- |
| Case-variant metadata | Capture rejects `.Grokptah/secret` and `.GIT/config`. | `case_variant_metadata_symlink_target_is_rejected` |
| Exact metadata and safe links | Exact `.git` and `.grokptah` targets stay rejected. A safe dangling internal target is retained. | `symlink_target_into_git_metadata_is_rejected`; `safe_dangling_internal_target_is_retained_exactly` |

Hosted Desktop for repaired functional SHA `d7306c1e7d346d1ee9ed6f36e06e457d5a65c6c6` only: GitHub Actions run `36055983316` attempt 1 (https://github.com/chriscase/GrokPtah/actions/runs/36055983316) concluded `success`. The event was `pull_request`. `head_sha` was `d7306c1e7d346d1ee9ed6f36e06e457d5a65c6c6`. The `desktop` job had no failed steps (`2026-09-24T20:36:19Z` to `2026-09-24T21:06:50Z`).

The commit that adds this section is documentation only. Its tree is not `a121de35fe498bc9fb147f53ccfa5f034949d95f`. A Desktop run for that docs tip is not the result above.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Scoped apply receipt transaction

Prior functional SHA: `d7306c1e7d346d1ee9ed6f36e06e457d5a65c6c6` (tree `a121de35fe498bc9fb147f53ccfa5f034949d95f`). This repair is functional SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad` (tree `2ade073c8ac6496c46d428a1b16993bc6d33b079`). It is not an independent acceptance of the frozen goal.

Apply cleanup sealing, receipt matching, and receipt completion resolve one receipt from the authenticated owner, session, and canonical workspace. The durable path is `idempotency/v2/{owner-digest}/{workspace-digest}/{request}.json`. They validate schema, owner, session, workspace digest, request ID, tool, payload hash, and expected status, and they do not scan the receipt tree. The same owner, session, and request ID in another workspace is a different file and stays byte-identical. The same workspace with another session still conflicts. An admission envelope is stored before either the pending receipt or the ApplySourceIntent changes, and it is removed only after both records match. A typed apply phase leaves the exact receipt recoverable once a source effect is possible. Store-open recovery completes that original receipt from AlreadyApplied, NotApplied, or Poisoned source classification before it clears the intent. Poisoned source keeps the intent and does not claim success or no effect.

`ccf776393988f2452b610a8aed77abbe66423076` (tree `01ad9cfcdcdb4d0c30ac3cce3384be77340bad33`) is an earlier publication of this repair. Its receipt path was only the owner shard and request ID, so another workspace with the same owner, session, and request ID occupied that file. Hosted Desktop run `36078044062` is the result for that SHA only. It is not the result for `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`.

Local validation on the working tree committed as `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad` (tree `2ade073c8ac6496c46d428a1b16993bc6d33b079`), before that commit. No executable diff exists after that commit. The named regressions below were `ok`. The live managed-executor test stayed ignored.

- Bridge `cargo fmt --all -- --check`: exit 0 (`FMT:0`), re-run for tree `2ade073c8ac6496c46d428a1b16993bc6d33b079` / SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`.
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0 (`CLIPPY:0`) on that same tree.
- Bridge `cargo test --locked -- --test-threads=1` on a fresh `GROKPTAH_HOME`: exit 0 (`SUITE:0`) on that same tree. Every required R1–R4 regression was `ok`: `foreign_owner_receipt_collision_is_ignored_and_unchanged`, `foreign_workspace_receipt_collision_is_ignored_and_unchanged`, `apply_recovery_completes_only_the_exact_scoped_receipt`, `receipt_lookup_does_not_scan_unrelated_owner_shards`, `admission_fault_cuts_reopen_without_a_permanent_in_progress_receipt`, `fault4_original_request_replays_recovered_success`, `fault5_original_request_replays_recovered_success`, `crash_after_intent_before_source_effect_resolves_original_request`, `not_applied_recovery_does_not_leave_a_permanent_pending_receipt`, `poisoned_recovery_never_replays_success_or_no_effect`, `repeated_store_reopen_is_idempotent_for_receipt_work_and_source`, `receipt_completion_precedes_intent_removal`. `live_grok_build_dogfood_runs_both_profiles_under_one_authority` stayed ignored.
- `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0 (`AUTHORITY:0`), re-run on the checked-out functional SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`.
- Isolated service `cargo test --locked -- --test-threads=1`: exit 0 (`SERVICE_TEST:0`).
- Isolated service `cargo check --locked --all-targets`: exit 0 (`SERVICE_CHECK:0`).
- Desktop `npm run typecheck`: exit 0 (`TC:0`), re-run on the checked-out functional SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`.
- Desktop `npm test`: exit 0 (`NPM:0`, 58 files, 428 tests), re-run on the checked-out functional SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`. These three results are for `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`, not for `ccf776393988f2452b610a8aed77abbe66423076`.
- `desktop/src-tauri` `cargo test --locked`: exit 0 (`TAURI:0`, 50 lib tests), re-run for tree `2ade073c8ac6496c46d428a1b16993bc6d33b079` / SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`.

| Gap | Result on `8cecd488` / `2ade073c` | Named regression |
| --- | --- | --- |
| R1 exact receipt | Seal, match, and complete one workspace-scoped receipt. A foreign owner or workspace collision stays unchanged and does not block the authorized apply. | `foreign_owner_receipt_collision_is_ignored_and_unchanged`; `foreign_workspace_receipt_collision_is_ignored_and_unchanged`; `apply_recovery_completes_only_the_exact_scoped_receipt`; `receipt_lookup_does_not_scan_unrelated_owner_shards` |
| R2 admission | Five cuts reopen to a terminal not-admitted receipt or a recoverable admitted pair. None stay permanently in progress. | `admission_fault_cuts_reopen_without_a_permanent_in_progress_receipt` |
| R3 original request | AlreadyApplied replays the original success. NotApplied is terminal. Poisoned does not replay success or no effect. | `fault4_original_request_replays_recovered_success`; `fault5_original_request_replays_recovered_success`; `crash_after_intent_before_source_effect_resolves_original_request`; `not_applied_recovery_does_not_leave_a_permanent_pending_receipt`; `poisoned_recovery_never_replays_success_or_no_effect`; `repeated_store_reopen_is_idempotent_for_receipt_work_and_source` |
| R4 intent retirement | The receipt is complete before the intent is removed. Replay converges. | `receipt_completion_precedes_intent_removal` |
| Accepted seals | Stale cleanup, foreign-path preservation, symlink containment, metadata case, and the approval-bound bundle stayed green. | `stale_cleanup_requires_the_approved_apply_bundle`; `foreign_file_at_candidate_add_path_is_preserved_and_discard_refuses`; `parent_component_after_symlink_cannot_escape`; `case_variant_metadata_symlink_target_is_rejected`; `resealed_patch_after_approval_cannot_apply` |

Hosted Desktop for repaired functional SHA `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad` only: GitHub Actions run `36083488303` attempt 1 (https://github.com/chriscase/GrokPtah/actions/runs/36083488303) concluded `success`. The event was `pull_request`. `head_sha` was `8cecd488ebb3f5b34692e70f09bb89cc2c8ec3ad`. The `desktop` job had no failed steps (`2026-09-25T01:46:55Z` to `2026-09-25T02:13:58Z`).

The commit that adds this correction is documentation only. Its tree is not `2ade073c8ac6496c46d428a1b16993bc6d33b079`. A Desktop run for that docs tip is not the result above.

Live-provider test: NOT RUN.

Production revocable xAI lease: STILL UNAVAILABLE.

## Repair identity

- Prior reviewed functional SHA: `32268e89f52776704d7a4729c2bd3581310ceeb7`
- Prior reviewed functional tree: `74727df90e187f906b1bdd912742073444978166`
- Repaired functional SHA: `bd4476cfd89b1e24f279d8883f01cfe4e74201a4`
- Repaired functional tree: `c1e1024a2908491f28023fe9c52a78eac93aa20c`
- This evidence section is documentation only once committed. It does not change executable code.

## Limitations

- No live Grok Build provider call. The ignored live managed-executor test was not run. `FileCredentialLease` remains test-only and does not revoke an already-read token. No production xAI lease authority is installed; an operator environment that names `GROKPTAH_MANAGED_GROK_EXECUTABLE` fails closed and dispatches no worker.
- `HostLeaseAuthority` is an in-process fake used by tests. Revocation rejects the already-read token at that fake provider. It is not a live provider proof.
- A mode-only change is applied and checked by `mode_only_change_applies_exactly`. The fake worker journey does not emit a mode-only edit.
- Check confinement uses macOS `sandbox-exec`. Non-macOS mutation remains refused.
- Application reuses `run_promotion` fingerprint, patch, and rollback helpers through `apply_recorded_patch`. It does not call `promote()` on a registered isolated worktree. The apply intent is a separate JSON record recovered on store open, not a second `WorkMutationIntent` decision ledger.
- A not-ready start of an already-admitted request id returns that work's status so a dirty post-apply tree can reconnect. A new not-ready request with no admission receipt still creates nothing.
- This file is not an independent acceptance of the frozen goal.
- The earlier desktop process drive used prepare and start. Settled review, apply, and discard are now on that same control plane and on the Work board. The Work board test exercises prepare without a selected agent, refresh of a settled diff, and apply of the displayed digest. The window itself was not clicked.
- At admission time the worker is still `running`, so the live start projection does not yet show `checksPassed`. The workflow test covers the settled candidate.
- Non-macOS mutation remains refused. This run is macOS.
- `processLimit` is not an enforced descendant cap. It is absent from the check-authority contract. The check supervisor proves the process group is gone or fails closed, and it enforces the output-directory byte limit during the run.
