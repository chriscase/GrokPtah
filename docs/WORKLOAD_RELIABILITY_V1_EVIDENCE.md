# Workload reliability v1 evidence

This is the requirement-to-proof record for the repair of the five adversarial findings on draft PR #584. It is not an independent review. It does not close #520, #521, #305, #301, or #492.

The baseline matrix stays in `docs/WORKLOAD_RELIABILITY_V1.md`. The evidence text published at `0cb77413066fbe94ef7b69a6fc886158b634e0fd` remains in git history. This file replaces it with the repair record.

## Identities

| Role | SHA | Tree |
| --- | --- | --- |
| PR base (`main` at review) | `c2b1c0b0ce4f968238d722d2ecccd2456a30f212` | `2c3f278fafe2e440e40d89dfcd66e13bc3e83cc3` |
| Reviewed published tip | `0cb77413066fbe94ef7b69a6fc886158b634e0fd` | `907e8c376216f38b32be2ce325afc69f6b5a551c` |
| Executable parent of that tip | `1e3d5f76a569b0213a90a2705777c26aa74a25e9` | `82dc8e8894731b29968cc655ae2ad95143de0abd` |
| Repair executable | `8c0e869112d61a00a60e230e8e09ce82df1552d4` | `8fc8381e7376aac0099964967314fea9a9235659` |
| First evidence tip | `e12a3692dcab8b9edfca7af0d7101ee1049770cc` | `d54452e1395b0b8c196ce346c766d1ce3888a38f` |

Only `docs/WORKLOAD_RELIABILITY_V1_EVIDENCE.md` differs between `1e3d5f76a` and `0cb77413`. The repair commits on top of `0cb77413` are:

- `9f9da8f3c` Bind each work receipt to the operation that commits it.
- `65970f82e` Persist reconciliation refusal before quarantining a journal.
- `493024ca1` Hold loaded Work Board pages across the background poll.
- `8c0e86911` Page nested attempt records under the existing transport ceiling.
- `e12a3692d` Record the workload reliability repair evidence.

`e12a3692d` changes only this file relative to `8c0e86911`. Hosted `desktop.yml` run `38200651982` completed with conclusion `success` on that exact SHA: https://github.com/chriscase/GrokPtah/actions/runs/38200651982. Event `pull_request`, 41m21s. Job `desktop` succeeded, including `Typecheck + unit tests`, `Cargo tests (desktop)`, `Bridge fmt + clippy + tests`, `Headless service fmt + clippy + tests`, and `Host authority spine (fmt + clippy + tests)`. No other workflow matched that push.

This commit records that terminal result. It also changes only this file. Rust, TypeScript, and the Tauri sources stay identical to `8c0e86911`. The hosted run for this newer tip is a separate check. Its URL is written on the PR description when that run reaches a terminal conclusion, because the URL does not exist until after this commit is published.

Review inputs, sha256, were not modified:

- `bc1c94309ebbcdab94b2ea3b074e614a3d29c8af6f5bd23430144e39ea11d6f6` adversarial review
- `ec364210c60d9b072d65cedc2fda0a155e26b71f87fd9968c372a97085b9e7ef` review evidence
- `7764306a812bbd183179569e7653013a61547f11e384dc5539b6bf02a60afbe9` original `adversarial_review_584.rs`
- `cfe7b18660badc2203fc87f24fcda421efbf6958248535b2ac08054828d4195d` original `review-workboard.cjs`
- `bd04ea692e6ab8306197c6c063a33890377e10ee2d53b3a909896fb6aad6c01f` original goal

The reviewed head was reproduced before editing. The unchanged Rust battery was 4 failed and 2 passed (the 4096/16384 graph proof and the child helper), exit 101. The Work Board harness lost loaded pages on the 2500 ms refresh, exit 1. Existing `workload_reliability_pages` was 5/5 and `native_executor_store` `process_crash` was 5/5. Those two passing suites are not evidence that the five defects were absent.

## Finding closure

| Finding | Production repair | Old failing proof | Repaired proof | Regression | Residual |
| --- | --- | --- | --- | --- | --- |
| F1 P1 shared armed request | `OrchStore::bind_work_request` installs a thread-local binding. `adopt_operation_request` takes it before the store lock. Drop clears a binding the transaction did not take. Background writers pass no binding. | `concurrent_bindings_must_not_publish_foreign_work_in_the_other_receipt`: arm A, arm B, A cancels. A's receipt stayed pending and B completed with A's work and `PRIVATE_OBJECTIVE_A`. | Same schedule on `bind_work_request`. `same_lane_service_requests_keep_their_own_receipts`, `different_lane_service_requests_keep_their_own_receipts`, and `bound_service_mutation_does_not_capture_an_unbound_lifecycle_writer` use barriers. | `tests/adversarial_review_584.rs` | The host maps every credential to one agent owner. Different owners are proven on the store. The service race uses two sessions, two workspaces, and two token ids. |
| F2 P1 quarantine before refusal | `quarantine_unbound_finalization_journal_unlocked` persists `ledger-reconciliation.json` and fsyncs it before the rename, then fsyncs both directories. Open also closes admission when any regular file remains in `managed-finalization-quarantine/`. | `failed_quarantine_marker_must_not_reopen_admission`: marker-write failure left the journal moved and the next open admitted. | Marker, destination, and directory-fsync faults, plus child exit 86 at `quarantine_before_marker`, `quarantine_after_marker`, and `quarantine_after_move`. Two further reopens keep the original bytes and the refusal. Claim, seal, close, and finalize stay denied. Authenticated `/ready` is 503. | Same test file. No marker-clear API was added. | A settled journal is still removed only when the managed intent is finalized or abandoned and the work state matches. An unresolved quarantine file is not treated as settled. |
| F3 P1 poll replaces loaded pages | `useWorkBoardPoll` calls `WorkBoardController.observe` every 2500 ms. `restart` is explicit Refresh, Retry, and post-mutation. An established continuation is held. Scope changes and unmount invalidate list and detail. | `review-workboard.cjs`: loaded continuation pages disappeared on the automatic refresh. | Adapted harness calls `WorkBoardController.observe`. Before and after the tick the loaded ids are `first`, `second`, calls are `FIRST` and `cursor-1`, and the selection stays `first`. | `useWorkBoardPoll.test.tsx` (13) and `WorkBoard.test.tsx` (10), included in desktop vitest. | Background observation does not poll every page. Explicit Refresh replaces the traversal. |
| F4 P1 claim receipt lost after the item write | `claim_work`, `renew_work_lease`, and `link_work_run` bind the original receipt. A committed claim replays through `lease_token_for_secret` and `token_matches`. The raw token is not stored. Renew and link use the `lease_view` shape. | `production_claim_recovery_must_complete_the_original_receipt`: child exit after the item write recovered Leased work and marked the original claim failed. | The production service caller, crash stages `lifecycle_after_intent`, `lifecycle_after_attempts`, `lifecycle_after_item`, `lifecycle_after_receipt`, and `lifecycle_after_commit`, then a fresh authenticated service. Renew and link are included. Two further reopens keep the original response. | Same test file. Verified-change settlement still completes a receipt only for `discard-candidate`, not for an intermediate lifecycle snapshot. | `create_work` is one `save_work_item` plus the outer receipt. No create-work intent was added. Credential rotation, wrong scope, expiry, stale revision, and payload conflict still refuse. |
| F5 P2 indivisible attempt view | `attempts` returns summaries. `attempt_detail`, `attempt_evidence`, and `attempt_artifacts` page the rest. The content fence is the SHA-256 of the public attempt view. The list fence covers attempt id plus that digest, in list order. | `maximal_valid_attempt_result_must_be_readable_through_detail`: one legal attempt envelope was 4273937 bytes and `limit=1` still exceeded 2097152. | The same create/claim/fail fixture. The wrapped full view is still 4273937. The nested walk maximum is 803817. Authenticated MCP wire page is 264027 and the maximum estimated envelope is 803767. `RemoteServiceClient::get_work` rebuilds every evidence string and artifact. | `workload_reliability_pages::maximal_attempt_detail_is_readable_over_authenticated_mcp` and `remote_client_reads_oversized_work_through_detail`. | Schema version stays 1. Ceilings stay 4096 items, 16384 edges, and 2097152 bytes. Heartbeat and run link move the attempt fence without a work-revision bump. |

## Commands and counts

Local machine, macOS arm64, rustc 1.92.0 (`ded5c06cf 2025-12-08`), cargo 1.92.0, node v25.2.1, npm 11.6.2. `CARGO_INCREMENTAL=0`. `RUSTC_WRAPPER` was unset. Local debuginfo stayed at the Cargo dev default. This is not the hosted sccache profile. Homes were temporary directories. The operator authority file was not modified.

Repair executable `8c0e86911`:

- Bridge `cargo fmt --check`: exit 0.
- Bridge `cargo clippy --locked --all-targets -- -D warnings`: exit 0.
- Bridge `cargo test --locked --offline -- --test-threads=1` with `GROKPTAH_HOME` set to a fresh directory: exit 0. 1323 passed, 0 failed, 15 ignored, 44 result blocks. The 15 ignored tests are the pre-existing installed-CLI, live-provider, doc, and lock-helper tests. No new ignore was added.
- Explicit `cargo test --locked --offline --test adversarial_review_584 -- --test-threads=1 --nocapture` on that same tree: exit 0, 16 passed, 0 failed, 0 ignored, 84.85s. Each test ended `ok`: `attempt_pages_follow_heartbeat_and_keep_every_nested_element`, `bound_service_mutation_does_not_capture_an_unbound_lifecycle_writer`, `claim_renew_and_link_cuts_replay_without_duplicate_effects`, `concurrent_bindings_must_not_publish_foreign_work_in_the_other_receipt`, `different_lane_service_requests_keep_their_own_receipts`, `failed_quarantine_directory_fsync_must_keep_moved_evidence`, `failed_quarantine_marker_must_not_reopen_admission`, `failed_quarantine_move_must_keep_the_marker_and_the_journal`, `full_item_and_edge_ceiling_graph_walk_is_lossless`, `legacy_quarantine_without_marker_stays_refused`, `maximal_valid_attempt_result_must_be_readable_through_detail`, `production_claim_crash_child`, `production_claim_recovery_must_complete_the_original_receipt`, `quarantine_crash_child`, `quarantine_process_cuts_preserve_evidence_and_refusal`, `same_lane_service_requests_keep_their_own_receipts`. Graph proof: 4096 nodes, 16384 edges, maximum estimated envelope 200256 bytes. One valid attempt wrapped body: 4273937 bytes, ceiling 2097152. Nested attempt page maximum: 803817 bytes.
- `cargo test --locked --offline --test workload_reliability_pages maximal_attempt_detail -- --nocapture`: exit 0. Wire page 264027 bytes. Maximum estimated envelope 803767 bytes.
- Included in the full bridge suite: `workload_reliability_pages` 6, `native_executor_store` 46, `native_executor_mcp` 16, `work_graph_authority` 25, `work_graph_service` 4, `workload_mcp` 1, `coordinator_mcp` 5, `coordinator_store` 12, `verified_change_workflow` 102, `provider_wire_conformance` 4.
- Test gateway, from `crates/codegen/grokptah-agent-bridge`, manifest `test-support/grokptah-test-gateway/Cargo.toml`: `cargo fmt --manifest-path test-support/grokptah-test-gateway/Cargo.toml --all -- --check` exit 0; `cargo clippy --manifest-path test-support/grokptah-test-gateway/Cargo.toml --locked --all-targets -- -D warnings` exit 0; `cargo test --manifest-path test-support/grokptah-test-gateway/Cargo.toml --locked` exit 0. 5 library, 11 integration, 1 doc, 0 failed.
- Service `cargo fmt --check`, `cargo check --locked --all-targets`, and clippy: exit 0. The first `cargo test --locked -- --test-threads=1` failed one test, `disconnect_reconnect_restart_and_cursor_expiry_are_durable`, while the bridge and Tauri suites were compiling. Shutdown reported `journal writer queue is full` and retained the instance lock, so the restart could not acquire it. The same suite rerun with no competing compile: exit 0. 4 library, 12 conformance, 5 smoke, 0 failed. That conformance test talks authenticated MCP to a disposable loopback service and replays a submit after a dropped connection.
- Desktop `npm run typecheck`: exit 0. `npm test`: exit 0, 60 files, 450 tests, including `useWorkBoardPoll.test.tsx` 13 and `WorkBoard.test.tsx` 10. `npm run build`: exit 0. Help public verification was not run. This repair does not change the help bundle.
- Tauri `cargo test --locked -- --test-threads=1` from `desktop/src-tauri`: exit 0. Library 52 passed, including `remote_client_reads_oversized_work_through_detail` and `remote_client_authenticates_and_reconnects_after_service_restart`. The binary target `src/main.rs` compiled and ran 0 tests. Doc-tests ran 0. This was not `--lib`.
- Root `cargo fmt -p xai-host-authority -- --check`: exit 0. Clippy exit 0. `cargo test -p xai-host-authority --locked -- --test-threads=1`: exit 0, 119 passed, 0 failed.
- `git diff --check`: exit 0.

The first bridge suite, without an isolated `GROKPTAH_HOME`, stopped in the library target: 756 passed, 3 failed, 11 ignored. The three failures were `transient_rate_limit_is_uncertain_and_is_not_retried`, `xai_qualification_forces_at_most_one_refresh_after_401`, and `xai_qualification_rejects_a_refresh_that_changes_principal`. The refresh test returned `missing field policy_revision` from the operator authority file. That file was not edited. The isolated rerun is the bridge result above.

Secret scan of the added lines from `c2b1c0b0` to `8c0e86911` under `crates/` and `desktop/` (10508 added lines): no private-key block, no `AKIA` key, no `ghp_` token, no `sk-` token, and no `xai-` live token. `leaseToken` appears in the live claim response after `redact_claim_lease_token` strips it from the persisted receipt, and in tests that assert the token is absent from pages, receipts, and error text. Crash-stage children exit 86. No production credential was read.

## Fault cuts

Child cuts use `GROKPTAH_CRASH_STAGE` and `process::exit(86)`.

- Lifecycle: `lifecycle_after_intent`, `lifecycle_after_attempts`, `lifecycle_after_item`, `lifecycle_after_receipt`, `lifecycle_after_commit`.
- Mutation: `mutation_after_intent`, `mutation_after_decision`, `mutation_after_item`, `mutation_after_receipt`, `mutation_after_commit`.
- Managed finalization: `managed_finalization_after_intent`, `managed_finalization_after_journal`.
- Quarantine: `quarantine_before_marker`, `quarantine_after_marker`, `quarantine_after_move`, `quarantine_after_durability`. The durability fault is the directory fsync after the rename. The marker write and the destination check use the existing durable-write fault.

## Wire, disk, and migration

Schema version stays 1. Kinds stay `grokptah.work.page.v1`, `grokptah.work.graph-page.v1`, and `grokptah.work.detail.v1`. List and graph consistency stays `version_fence`. Attempt consistency is `attempt_fence`. Limits stay 4096 items, 16384 edges, and 2097152 bytes. Default list limit stays 50, maximum 200. Detail default stays 32, maximum 64.

`ptah_get_work_detail` accepts optional `attempt_id` and `consistency_fence`. New sections are `attempt_detail`, `attempt_evidence`, and `attempt_artifacts`. The `attempts` section now returns summaries: public attempt scalars, result metadata, `evidenceCount`, `artifactCount`, and `fence`. It does not return the evidence strings or artifact objects. A client of the pre-repair draft that decoded `attempts` as full `WorkAttemptView` values must walk the three nested sections. `WorkDetailPageV1` adds optional `attemptId` and `consistencyFence`. Old pages without those fields still decode. Offset 0 may omit the fence and receives the current one. A later page, or a different fence, returns `restart_required` and no elements.

Legacy `ptah_get_work` is unchanged: `{work, attempts}` when the body fits, `capacity_exhausted` when it does not. `RemoteServiceClient::get_work` still uses that tool first and falls back to detail only for `capacity_exhausted`. The fallback now reconstructs each attempt from the nested pages. Lease fields stay out of every page.

Disk attempt files are unchanged. No attempt migration runs. `ledger-reconciliation.json` stays `{"schemaVersion":1,"code":"unbound_managed_finalization"}`. It is written and fsynced before a quarantine rename. A directory that already contains quarantine evidence closes admission on open even when the marker is missing, and the marker is then persisted. There is no marker-clear API. The shared `armed_request` slot is gone. Existing intent files that already carry a request binding still replay. Intents without one stay unbound.

## Reviewer map

- Request binding: `OrchStore::bind_work_request` and `adopt_operation_request` in `orchestration/store.rs`. Service callers bind in `orchestration/service.rs` (`bind_work_request` near the claim path, `claim_work`, `link_work_run`, and `work_lease_mutation` for renew). Validation checks the pending idempotency receipt, owner, session, workspace digest, tool, payload hash, target work, target attempt, and expected revision. The test gate waits inside `bind_work_request`, not inside adopt, so a supervisor cannot consume the barrier.
- Original response recovery: `commit_lifecycle_with_request_unlocked` persists the intent before the attempt, item, and receipt. Open runs `recover_work_lifecycle_intents` before orphaned-receipt failure. Claim replay restores the lease with `lease_token_for_secret` and `token_matches`. Empty or rotated secrets refuse without rewriting a complete receipt. `lifecycle_settles_service_receipt` still withholds settlement for intermediate verified-change snapshots.
- Quarantine order: `quarantine_unbound_finalization_journal_unlocked`. Marker, then destination check, directory create, rename, source-directory fsync, quarantine-directory fsync. `load_ledger_reconciliation_marker` also treats a quarantine file as unresolved evidence.
- UI poll: `desktop/src/lib/useWorkBoardPoll.ts` interval `WORK_BOARD_POLL_MS` (2500) calls `observe`. `WorkBoardController.restart` replaces the list. `loadMore` appends. A fence change or expired cursor sets refresh-required and does not mix the new first page into old pages.
- Nested attempts: `attempt_content_fence` and `project_work_detail` in `orchestration/work_page.rs`. MCP arguments in `mcp_control.rs`. Remote reconstruction is `collect_attempt_views` in `desktop/src-tauri/src/remote_service.rs`.

## Local versus hosted

Hosted Desktop success on `0cb77413` (run `38178900645`) does not cover this repair. Local commands above were run on `8c0e86911`. No live provider, production credential, Computer Use session, or native input was used. Loopback and synthetic tokens supplied the service and MCP proofs.

Hosted `desktop.yml` on the first evidence tip `e12a3692d` is terminal success, run `38200651982`. That run used the hosted normal profile, hosted sccache, and the hosted macOS `cargo test --locked` for the Tauri binary. It is not a result for a later documentation SHA. The 15 ignored bridge tests were not promoted into this claim.
