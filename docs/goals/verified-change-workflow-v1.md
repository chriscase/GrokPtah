# Verified Change Workflow v1

Status: frozen implementation goal. Later edits to the acceptance criteria are amendments in this file, not silent weakenings.

## Base

- Repository: `chriscase/GrokPtah`
- Recorded `origin/main` SHA: `0dbe51c8aa94e3f26543be7a90e0085029487de7`
- Recorded `origin/main` tree: `7fff3368c12b2deffc3d1387d6c40019f09bb7d2`
- Subject: `Merge pull request #578 from chriscase/cursor/cb-v0-live-open-panel-deny-e1ab`
- Fetched 2026-09-22. Main had not advanced past the review baseline.
- Implementation branch: `grok/verified-change-workflow-v1`
- Isolated worktree: created from that exact revision. The developer's checkout was not used.

## Existing behavior

The shared in-process bridge already owns Work, attempts, managed execution, and the Grok Build adapter.

- Durable assignment and the crash-recovery envelope for a decision plus its work item live in `crates/codegen/grokptah-agent-bridge/src/orchestration/store.rs` (`WorkMutationIntent`). Issue #521's split write is already repaired there. Regression: `work_mutation_intent_recovers_after_decision_only_crash`, `work_mutation_intent_recovers_after_item_only_crash`, `work_mutation_intent_refuses_an_unexpected_prior_revision`, `work_mutation_intent_binds_decision_to_expected_prior_revision`.
- Managed Grok Build admission, one supervised attempt, and isolated review live in `src/orchestration/service.rs` and `src/grok_build.rs`.
- Issue #504 / PR #560 already demonstrated Economy and High Assurance reaching `AwaitingApproval` after a disposable `DOGFOOD.txt` edit. That demonstration is prior work. It is not this goal's success case.
- Completion success for ordinary work is `evidence_authorizes_success` plus `OrchStore::success_is_authorized_unlocked`. A clean model verdict is not, by itself, host verification of a declared check.
- Approval (`approve_work`) and run promotion/discard already exist. Passing checks do not approve. Before this goal, a successful isolated-review mutation was copied onto the source workspace before the human approval.

## Gaps this implementation closes

1. An operator prepare/start entry on the existing Work/Run service, desktop command, and Work board, without a second mission store.
2. Readiness that inspects the installed CLI `inspect --json` contract and declared check executables, records version and capability, spends no provider tokens, and dispatches zero workers when the CLI, platform, mode, or toolchain is unavailable. Read-only mode and non-macOS mutation stay fail-closed. The nested CLI version is not taken from the agent model route. The child environment stays restricted.
3. One supervised managed-work attempt that repairs two source files. A host-owned oracle outside the checkout is red before the repair and green only on the retained candidate.
4. Candidate content identity, including untracked source and excluding build outputs, bound to Work/attempt and the required-check spec. Failed, missing, skipped, truncated, or incomplete checks cannot verify. A later candidate edit invalidates verification and approval.
5. Approval stays distinct from application. Apply and discard name the exact candidate digest. Discard does not change the source workspace. Demonstrations use disposable repositories.
6. Restart reopens the production store. A dispatch that is no longer supervised stays non-success. Reconnect does not admit a second attempt. Cancellation wins over a late child completion.

## Acceptance criteria

### G1 — A real operator entry point

Extend the existing Work/Run product surfaces to let an operator prepare and start one bounded Grok Build coding assignment.

Reuse existing durable records and authority. Do not create a parallel mission store, provider ledger, scheduler, or competing SDK.

The operator must be able to understand:

- repository and source revision;
- assigned Agent and execution host;
- allowed change scope;
- selected executor/model configuration;
- execution limits;
- required checks;
- approval requirements;
- readiness and concrete reasons execution is unavailable.

If parts already exist, integrate and test them rather than rebuilding them. The end-to-end journey must not depend on manually constructing hidden DTOs, editing durable JSON, or invoking a test-only helper.

### G2 — Truthful readiness before dispatch

Check the actual installed CLI and supported inspection/output contract. Record versions and capability evidence without exposing credentials.

Do not assume the user's selected 4.7 implementation model determines the version or behavior of a nested managed Grok Build CLI.

Preserve the existing fail-closed behavior for unsupported execution modes. Do not remove the ReadOnly refusal or enable non-macOS mutation merely to make the demonstration pass.

Check required toolchain/validator availability explicitly. The adapter uses a restricted environment; do not solve missing tools by forwarding the operator's entire PATH, HOME, environment, or credentials.

Readiness inspection must not itself spend provider tokens or launch work. Missing readiness produces an actionable state, not a misleading ready flag.

### G3 — Meaningful bounded implementation

Execute through the production managed-work path, not merely by calling the low-level adapter from a bespoke demonstration.

Use disposable repositories/checkouts and explicit allowed-file scope. Preserve source-identity checks, private child Git state, credential handling, and the absence of child publishing authority.

Demonstrate a behavioral task requiring coordinated edits to at least two source files. Include a deterministic regression that fails before the repair and passes after it.

Do not use DOGFOOD.txt, a version-string change, or a documentation-only edit as the primary success demonstration.

Keep required acceptance checks outside the worker's writable scope, or otherwise establish an equally strong host-controlled test oracle. The worker must not succeed by deleting, weakening, skipping, or rewriting its own oracle.

Start with one supervised worker attempt. Do not add parallel-worker scheduling or implicit retry to this goal.

### G4 — Verification bound to the candidate

Reuse and strengthen existing completion evidence as necessary.

The system must distinguish:

- the worker stopped;
- the worker proposed a change;
- required checks passed for that candidate;
- a human approved the candidate;
- the candidate was actually applied.

A clean model verdict, successful CLI exit, or generic observed test command is insufficient by itself.

Execute declared required checks through a host-controlled, bounded mechanism or prove the existing mechanism already establishes equivalent guarantees. Use explicit executable/argument/cwd/environment and resource limits; do not add an unrestricted frontend-supplied shell authority.

Bind evidence to:

- Work/Run/Attempt identity;
- source revision and exact candidate-content identity;
- required-check specification;
- commands/checks actually executed;
- exit/outcome and timeout/cancellation;
- bounded artifact references.

Required checks must apply to the final candidate, not a pre-edit snapshot. A subsequent source edit invalidates verification and approval. Account for untracked source files and relevant path/symlink behavior. Separate generated build outputs from the candidate's source identity.

Missing checks, skipped required checks, failed checks, truncated evidence, or an incomplete worker result cannot become verified success.

Do not rewrite the entire completion subsystem. Establish the invariant at the existing canonical boundary and test all entry points that use it.

### G5 — Review, accept, and discard

Present the candidate diff, required-check results, relevant limits, and honest completion state through the existing operator experience.

Reuse existing approval/promotion/discard semantics. Passing checks must not automatically approve or promote a change.

Keep review approval and application distinct. Reject stale approval after the candidate or target changes. Discard must leave the original source workspace unchanged.

Use disposable source targets for acceptance/promotion demonstrations. This goal does not authorize promotion into the developer's real main branch.

### G6 — Durable recovery and cancellation

Exercise production persistence and a real process restart.

Recover enough state to show:

- what was requested;
- whether execution was admitted;
- which attempt/candidate exists;
- whether verification finished;
- whether approval/application happened;
- what action is now safe.

Restart or reconnect must not silently dispatch a duplicate paid attempt. Ambiguous execution must remain explicit until existing reconciliation or operator action resolves it.

Cancellation must prevent later success/promotion and must not claim process termination or cleanup before those facts are established. Preserve quarantine/revocation behavior when termination is uncertain.

Reproduce #521's relevant crash concern against current code. If already repaired, cite the current implementation and regression. If it remains reachable on this journey, repair it within the canonical durable mutation mechanism; do not create another decision ledger.

### G7 — Production-grounded acceptance evidence

Provide deterministic tests covering at least:

1. Meaningful multi-file repair: required check red before, green after.
2. Failed/missing/skipped required checks cannot verify a candidate.
3. Post-verification mutation invalidates verification and approval.
4. Out-of-scope and symlink/path escape attempts are rejected.
5. Cancellation plus late completion cannot produce success/application.
6. Restart at admission, candidate persistence, verification, and approval/application boundaries has truthful recovery.
7. Duplicate request/reconnect does not dispatch duplicate work.
8. Unsupported/missing readiness performs zero worker dispatch.
9. Accept and discard operate only on the intended exact candidate.
10. Public projections and published evidence contain no credentials.

Test through the actual host/service and desktop-facing boundaries where those boundaries are part of the implementation. Do not substitute a mock UI demonstration or an in-memory store for the claimed production journey.

Run relevant existing regression suites, not only new focused tests. Derive exact commands from current manifests and CI. Respect nested Rust workspaces and the upstream-generated root manifest. Do not disable gates, hide baseline failures, or relabel skips as passes.

Use fake providers/CLI fixtures for ordinary CI. Run a bounded live demonstration only under an existing explicit live-use grant and the supported platform/configuration. Do not discover and reuse ambient credentials or add paid services.

If live execution is unavailable, complete the implementable work and report that qualification gap honestly. Do not claim the full live exit gate passed.

### G8 — Published, independently reviewable result

Commit coherent increments and normally push the feature branch. Open/update one draft PR describing this goal.

Publish a compact, sanitized evidence index containing:

- each acceptance ID;
- implementation paths;
- test or demonstration commands;
- actual results;
- execution environment;
- evidence/artifact references;
- qualification class and limitations.

Keep secrets, auth files, private transcripts, personal paths, and bulky raw logs out of Git. Do not publish a digest as though it lets a reviewer inspect an otherwise unavailable artifact.

Record the tested functional SHA/tree and the final published SHA/tree. If a later commit only adds evidence documentation, explain that relationship. Any later executable change requires applicable revalidation.

Verify that origin resolves to the reported final branch SHA. A local-only commit is not a completed handoff.

## Permitted scope

- `docs/goals/verified-change-workflow-v1.md` and `docs/goals/verified-change-workflow-v1-evidence.md`
- `crates/codegen/grokptah-agent-bridge` readiness, candidate identity, required checks, managed-work finalization, approval, apply, and discard
- Desktop command and Work-board projection that call those service methods
- Tests and the draft PR for this branch

## Non-goals

- Whole-branch imports of historical runtime or authority stacks
- A new provider, send, or task authority
- Multi-agent swarm expansion
- Arbitrary Computer Use or contained-browser admission
- Linux/Windows sandbox implementation
- A broad UI redesign
- Automatic merge, release, deployment, or backlog closure
- Weakening ReadOnly or non-macOS fail-closed behavior
- Promoting into the developer's real main branch
- Replaying the #504 `DOGFOOD.txt` demonstration as this goal

## Baseline

Commands run on the recorded main SHA before this goal's code:

```sh
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib work_mutation_intent -- --test-threads=1
```

Result: 4 passed, exit 0. These are the #521 regressions.

```sh
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test grok_build_adapter --test grok_build_managed_executor -- \
  --test-threads=1 --skip live_grok_build_dogfood
```

Result: adapter 27 passed; managed executor 1 passed and 1 ignored (`live_grok_build_dogfood_runs_both_profiles_under_one_authority`). Exit 0.

No pre-existing failure was observed in that focused set. The ignored live test is not a pass. Root-wide `cargo test` remains unsupported per `docs/VERIFICATION.md`.

## Donors

No historical branch was checked out or merged.

| Donor | SHA | Disposition |
| --- | --- | --- |
| `origin/main` at the freeze above | `0dbe51c8aa94e3f26543be7a90e0085029487de7` | starting point |
| PR #560 reviewed head, already merged | `993ce5e6d7c4edf3a690acb00a9d22d5a2d69527` | already-present. Not replayed. The DOGFOOD.txt journey stays a prior demonstration. |
| Issue #504 adapter note `grok/self-host-grok-adapter-v1` | `718835656ab49cbfef729437ddac43b7ab5810cd` | drop as an import. The current main adapter is the code under change. |
| Issue #504 authority note `codex/self-host-work-authority-v1` | `d62d7a8b7a19fd1f514397879c0725fe3104c0f6` | drop as an import. Assignment mutation recovery on main is the #521 fix. |

## Amendment

None.
