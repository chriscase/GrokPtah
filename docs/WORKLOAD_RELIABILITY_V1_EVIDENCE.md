# Workload reliability v1 evidence

This is the requirement-to-proof record, the local evidence bundle, and the reviewer map for the implementation on `grok/workload-reliability-v1`. It is not an independent review. It does not close #520, #521, #305, #301, or #492.

The baseline matrix and the red 4096-item observation stay in `docs/WORKLOAD_RELIABILITY_V1.md`.

## Proven on this working tree before the evidence commit

Base: `c2b1c0b0ce4f968238d722d2ecccd2456a30f212`, tree `2c3f278fafe2e440e40d89dfcd66e13bc3e83cc3`. That SHA still matched `origin/main` when these commands ran.

Executable code commit: `4903862f0` (`Qualify workload crash recovery on the production write path`). The two evidence commits after it change only this file. Rust, TypeScript, and schema bytes match `4903862f0`.

Host: macOS 26.3, arm64. Toolchains: rustc 1.92.0, cargo 1.92.0, rustfmt 1.8.0-stable, node v25.2.1, npm 11.6.2, git 2.50.1. Bridge and service tests used an isolated `GROKPTAH_HOME` and `CARGO_INCREMENTAL=0`. No production credentials, live provider, or packaged app.

| Gate | Command | Result |
| --- | --- | --- |
| Bridge fmt | `cargo fmt --check` in `grokptah-agent-bridge` | exit 0 |
| Bridge clippy | `cargo clippy --locked --offline --all-targets -- -D warnings` | exit 0, 13.87s |
| Bridge tests | `cargo test --locked --offline -- --test-threads=1` | exit 0. 1304 passed, 0 failed, 15 ignored, 43 binaries |
| Service fmt | `cargo fmt --check` in `grokptah-service` | exit 0 |
| Service clippy | `cargo clippy --locked --all-targets -- -D warnings` | exit 0, before the service suite, service sources unchanged after it |
| Service tests | `cargo test --locked --offline -- --test-threads=1` | exit 0. 21 passed, 0 failed, 0 ignored |
| Test gateway | fmt `--check`, clippy `-D warnings`, `cargo test --locked --offline` | exit 0. 11 tests + 1 doc test passed |
| Desktop typecheck, vitest, production build | `npm` scripts in `desktop/` | exit 0. vitest 59 files, 436 tests. Build 2.15s |
| `git diff --check` | worktree before commits | clean |

The 15 ignored bridge tests are the pre-existing installed-CLI, live Grok Build, lock-holder child, and `DurableWriteGuard` doc test. This change adds no `#[ignore]`.

`git diff --check` was clean on the uncommitted tree. Hosted `desktop.yml` on the published head is recorded in the PR after that run exists. Pending is not success.

## Requirement to proof

| Requirement | Caller | Proof | Result |
| --- | --- | --- | --- |
| Traverse 0 through 4096 | `select_work_list_page`, `ptah_list_work_page`, `ptah_get_work_graph_page` | `workload_reliability_pages` (5) and `legal_ceiling_list_and_graph_exceed_the_mcp_body_ceiling` | passed. Legacy list/graph of 4096 return `MCP remote error: capacity_exhausted` and do not truncate. The lane is walked by the paged tools |
| Ties, empty filter, UTF-8, foreign deps | same selector | `lane_boundaries_filters_utf8_and_foreign_dependencies`, `bounded_pages_cover_small_lanes_and_reject_bad_continuations` | passed. 128 dependencies, 64 foreign, unresolved count only |
| Version fence | cursor digest in `work_page.rs` | reprioritize between pages returns `restart_required` | passed |
| Cursor rejection before data | `open_cursor` | tamper, expiry, unknown version, foreign scope, foreign reader | passed in the page tests |
| Byte budget | `tools_call_response_len` vs the raw tools/call body | one authenticated response's byte length equals the estimator and is `<=` 2 MiB (`2097152`) | passed. The numeric length is the equality, not a stored constant |
| Maximal item detail | `ptah_get_work_detail` | `oversized_item_detail_is_pageable_when_get_by_id_refuses` | passed. Legacy get refuses `capacity_exhausted` |
| Standalone MCP service | `hosted_service_pages_one_work_lane_the_same_way_twice` | two authenticated clients, same page and full walk, graph agrees, legacy list of 3, zero runs | passed, 12.50s inside the service suite |
| Desktop board states | `workTraversal.ts`, `WorkBoard.tsx` | vitest, including lane generation and partial counts | 436 passed. Counts on a partial lane are loaded counts |
| Remote adapter | `remote_service.rs` `list_work_page` | `remote_client_follows_work_pages_and_refuses_a_changed_lane` | not executed locally. Source is in the branch. Hosted Tauri `cargo test --locked` is the macOS gate |
| Managed finalization binding | `commit_work_lifecycle_intent_unlocked`, `apply_managed_finalization_unlocked` | child cuts `managed_finalization_after_intent` and `managed_finalization_after_journal`; in-process stage cuts; review stays out of execution | `native_executor_store` 46 passed |
| Queued seal | `seal_queued_managed_work` | `process_crash_seal_queued_managed_work_commits_once` | passed inside that binary |
| Assignment and lifecycle | offer/fail production writes | `process_crash_offer_commits_one_decision_and_replays`, `process_crash_fail_commits_one_attempt_and_replays` | passed. Replay is stable across two further reopens. `effect_counts` stays `(0, 0)` for `verified-changes/` and `runs/` |
| Legacy journal | `quarantine_unbound_finalization_journal_unlocked` | `legacy_journal_without_envelope_is_quarantined` | passed. Open succeeds, claim is the fixed reconciliation conflict, evidence is kept |
| Same-process tear | `note_partial_commit`, `/ready` | `same_process_item_write_failure_closes_admission_until_reopen`, `write_fault_makes_the_live_server_unready_until_reopen` | passed. Authenticated `/ready` is 503 with the fixed message and no path. Reopen after ordered shutdown is ready |
| Verified Change receipts | `lifecycle_settles_service_receipt` | `verified_change_workflow` 102 passed after apply/discard receipts stopped being completed inside intermediate lifecycle snapshots | passed, 302.34s |
| Preserve earlier suites | coordinator, workload store, graph authority, provider qualification under isolated home | included in the 1304 | passed. Provider qualification had failed earlier against the operator home's authority file (`missing field policy_revision`). That file was not modified. The isolated-home rerun passed those three tests |

## Wire and disk compatibility

- Additive tools: `ptah_list_work_page`, `ptah_get_work_graph_page`, `ptah_get_work_detail`. Legacy tools keep `{work}`, `{graph}`, and `{work, attempts}`.
- A legacy body that would exceed 2 MiB returns `capacity_exhausted` and names the paged tool. The 2 MiB ceiling, 4096-item ceiling, and 16384-edge ceiling are unchanged.
- Page order: priority descending, `created_at` ascending, `work_id` ascending. Default limit 50, maximum 200. Preferred envelope 256 KiB. One legal entry may exceed the preferred target when it still fits under 2 MiB.
- Consistency is a version fence. A lane digest mismatch returns `restart_required` and no items. Deadline admission uses the sealed `admitted_at` inside the cursor.
- The cursor is HMAC-SHA256 with a host-owned `page-mac.key` (mode 0600). The token body has no workspace path, lease, or credential.
- List rows are `WorkListEntryV1`. Graph nodes stay the existing redacted projection. Foreign dependency ids stay an unresolved count.
- Managed finalization writes the lifecycle envelope, including prior and successor work and attempts, before the journal. Recovery adopts that envelope. A journal with no authentic envelope is moved aside and `ledger-reconciliation.json` records `unbound_managed_finalization` with no path. Admission closes with `ledger reconciliation is required`.
- Apply and discard verified-change receipts are not settled by an intermediate `apply-candidate` or `apply-reconciliation` lifecycle snapshot. Discard receipts settle on `discard-candidate`. Apply receipts stay with `complete_pending_apply_receipt_unlocked` and `fail_exact_apply_receipt_unlocked`.
- `begin_managed_permission_resolve` and `abort_managed_permission_resolve` remain single-record writes.

## Crash cuts that passed

Child process exit 86 at `GROKPTAH_CRASH_STAGE`, then a fresh store open, replay of the original request, then two more opens:

- `mutation_after_decision` for an offer (one decision, stable replay)
- lifecycle fail after the attempt/item/receipt cuts used by `process_crash_fail_commits_one_attempt_and_replays`
- `managed_finalization_after_intent` and `managed_finalization_after_journal` (awaiting review is not released)
- queued managed seal across the intent/write/cleanup boundary in `process_crash_seal_queued_managed_work_commits_once`

In-process stage returns cover `BeforeJournal`, `AfterIntent`, `AfterJournal`, `AfterAttempt`, and `AfterWork` for managed finalization. A same-process fault on the `work-items` write closes admission until reopen.

## Privacy scan

Synthetic and offline. The page tests refuse responses and errors that contain the workspace display path, the canonical path, the bearer, or a foreign dependency id. This does not qualify a live provider or a packaged native build.

## Reviewer map

Changed API: `ptah_list_work_page`, `ptah_get_work_graph_page`, `ptah_get_work_detail`; error code `restart_required`; types `WorkListPageV1`, `WorkGraphPageV1`, `WorkDetailPageV1`.

Store commit and recovery: `commit_work_lifecycle_intent_unlocked`, `lifecycle_settles_service_receipt`, `complete_bound_request_unlocked`, `recover_work_lifecycle_intents`, `apply_managed_finalization_unlocked`, `quarantine_unbound_finalization_journal_unlocked`, `note_partial_commit`, `load_or_create_page_mac_key`.

Selectors: `select_work_list_page`, `select_work_graph_page`, `project_work_detail` in `orchestration/work_page.rs`. MCP handlers only call those. Desktop `work_list` and `RemoteServiceClient::list_work_page` do the same.

Fault hooks, test-only: `GROKPTAH_CRASH_STAGE` (`process::exit(86)`), `DURABLE_WRITE_FAULT` (existing verified-change needle), `APPLY_FAULT`, `CLEANUP_FAULT`.

Highest risk:

1. A lifecycle snapshot inside verified-change apply must not complete that receipt. `lifecycle_settles_service_receipt` is the guard. The 102 verified-change tests are the regression net.
2. Version-fence restarts are correct when reconciliation rewrites dependency state. A long limit-1 graph walk can cross the 5 second supervisor tick and return `restart_required`. The product does that on purpose. The unchanged-lane proof stays inside one interval.
3. `page_mac_key` loss invalidates every continuation. Callers must restart. The key is not in the cursor.
4. Legacy unbounded tools still build the full body and then refuse. They do not truncate, and they are not the traversal path.
5. Remote Work Board paging is implemented and unproven on a local Tauri binary.

## Unresolved on this tree

- Local `desktop/src-tauri` `cargo test --locked` did not run. An earlier debug compile died with no space left, and that incomplete target was removed. Hosted `desktop.yml` is the macOS compile and test gate.
- A lane of 16384 edges was not materialized. The edge ceiling was not lowered. The covered graph is one item with 128 dependencies.
- Not every production writer has its own child-process cut. Block, renew, progress, approval, and reconciliation rely on the shared lifecycle/mutation envelope plus the existing store tests.
- The portable SDK (`grokptah-agent-sdk`) does not expose list/graph work tools, so it was not given a second client.
- Hosted workflow status is unknown until the draft PR's head is checked. Earlier local success is not that status.
