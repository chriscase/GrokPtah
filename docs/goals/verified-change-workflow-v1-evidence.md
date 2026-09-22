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

## Limitations

- No live Grok Build provider call. The ignored live managed-executor test was not run.
- The earlier desktop process drive used prepare and start. Settled review, apply, and discard are now on that same control plane and on the Work board. The Work board test exercises prepare without a selected agent, refresh of a settled diff, and apply of the displayed digest. The window itself was not clicked.
- At admission time the worker is still `running`, so the live start projection does not yet show `checksPassed`. The workflow test covers the settled candidate.
- Non-macOS mutation remains refused. This run is macOS.
