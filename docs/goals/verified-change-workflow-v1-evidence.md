# Verified Change Workflow v1 evidence

Qualification class: fixture and local CI. Not a live provider run. Not a packaged, notarized, or Computer Use claim.

Environment: macOS 26.3, arm64, rustc 1.92.0. Fake Grok CLI and disposable git repositories. No operator PATH, HOME, or GitHub token is forwarded to the child. The published projection is scanned for the fixture credential and it is absent.

The live exit gate was not run. No explicit live-use grant was present. The ignored managed-executor live test was not executed. Issue #504 and PR #560 remain the earlier DOGFOOD.txt demonstration, not this result.

## G1

Operator prepare and start are `OrchestrationService::prepare_verified_change` and `start_verified_change`. They create one manager plan and one Work item on the existing ledger, assign the existing agent, and authorize execution. Desktop commands `verified_change_prepare` and `verified_change_start` call those methods. The Work board form renders repository, revision, agent, host, scope, executor, model, CLI version, limits, required checks, approval, readiness reasons, and phases.

Paths: `crates/codegen/grokptah-agent-bridge/src/orchestration/service.rs`, `desktop/src-tauri/src/commands.rs`, `desktop/src/components/VerifiedChangePanel.tsx`, `desktop/src/components/WorkBoard.tsx`.

Command: `cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --test verified_change_workflow -- --test-threads=1`

Result: `multi_file_repair_is_red_then_green_and_apply_is_separate` passed. The projection asserts repository id, source revision, agent id, execution host, both allowed files, executor, distinct model key and CLI version `1.0.5`, limits, and approval required.

Desktop: `npm test -- --run src/components/VerifiedChangePanel.test.tsx src/components/WorkBoard.test.tsx` passed (5). `npm run typecheck` passed. `desktop/src-tauri` `cargo test --locked --lib -- --test-threads=1` passed 50.

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

Result: after approval the source pair is still the pre-repair text. Apply then matches the repaired pair. Discard of the applied digest errors and leaves the repaired pair in place. The earlier unapplied candidate is not applied by that call.

## G6

Restart reopens the same orchestration store. The reviewed work stays `AwaitingApproval` with the same attempt count and an unapplied source. Cancellation of an in-flight hold, followed by the child's late completion, stays `cancelled` and does not change the source.

Issue #521 is already repaired on the base by `WorkMutationIntent` in `store.rs`. The assignment path used here goes through that mutation. Regressions on the base, re-run in spirit by the store tests present on main: `work_mutation_intent_recovers_after_decision_only_crash` and the sibling intent tests, 4 passed at freeze. This goal did not add a second decision ledger.

Result: `cancel_and_restart_do_not_dispatch_or_apply` passed on two consecutive runs.

## G7

| # | Case | Result |
| --- | --- | --- |
| 1 | Multi-file red then green | pass, workflow test |
| 2 | Failed, missing, or skipped checks | pass, noop plus unit test |
| 3 | Post-verification mutation | pass, tamper then approve fails |
| 4 | Out-of-scope and symlink | pass, escape and symlink tests plus identity unit test |
| 5 | Cancel plus late completion | pass |
| 6 | Restart | pass for candidate persistence and verification. Admission without a live task remains the existing uncertain review path. |
| 7 | Duplicate start | pass, one attempt |
| 8 | Unsupported readiness | pass, zero intents |
| 9 | Exact candidate apply and discard | pass |
| 10 | No credentials in the projection | pass, `assert_secret_free` |

Bridge clippy `--locked --all-targets -- -D warnings`: exit 0. `cargo fmt --all`: exit 0.

Regression: adapter 27 passed. Managed executor non-live test 1 passed. Live test ignored, not counted as a pass.

## G8

See the draft PR and this file. Tested functional tree is the commit that contains the executable changes. A following commit that only updates this evidence file does not change behavior.

## Limitations

- No live Grok Build provider call.
- Desktop GUI process starts, then was stopped before an operator clicked Prepare or Start. Panel tests and the service tests cover the projection and the assignment entry. The desktop lib suite passed (50).
- Non-macOS mutation remains refused. This run is macOS.
