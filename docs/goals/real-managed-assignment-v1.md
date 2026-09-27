# GrokPtah Real Managed Assignment v1

The previous Verified Change Workflow v1 has completed independent review. Do not continue repairing or extending that historical implementation branch.

## Mission

Starting from the latest `origin/main` after PR #579 is integrated, prove that GrokPtah can perform one **real, bounded, operator-initiated Grok Build software-engineering assignment** through the production managed-execution path and return a durable candidate that GrokPtah can independently verify, review, approve/discard, and recover across restart.

This is the transition from a thoroughly tested workflow foundation to a usable self-hosting product capability.

The goal is NOT merely to invoke a CLI.

The goal is:

**operator assignment → truthful readiness → bounded Grok Build execution → meaningful repository change → retained candidate → host-controlled verification → durable review state → explicit operator disposition**

using the production GrokPtah architecture.

---

# 0. Establish identity before implementation

Refresh `origin/main`.

Record:

- repository
- branch
- base SHA
- base tree
- current `origin/main` SHA/tree
- PR #579 merge SHA if present
- relevant existing Grok Build adapter/executor paths
- relevant Work/Run/Attempt and Verified Change paths
- existing credential/provider-authority paths
- exact installed Grok Build CLI/version/capabilities

Create a fresh implementation branch/worktree.

Suggested branch:

`grok/real-managed-assignment-v1`

Do not reuse the #579 worktree or branch.

Create:

`docs/goals/real-managed-assignment-v1.md`

Commit and push the goal document early.

Once committed, treat its acceptance criteria as frozen. Clarifications may be appended, but criteria may not be silently weakened.

---

# 1. Reconcile current production truth

Before changing code, trace the actual current-main execution path.

Document:

1. operator entry point
2. Work creation and assignment
3. execution approval
4. executor selection
5. Grok Build launch
6. credential acquisition
7. process/environment isolation
8. repository/worktree isolation
9. completion evidence
10. candidate retention
11. host-controlled verification
12. review/approval
13. source application
14. restart recovery
15. cancellation
16. late-result handling

Do not create parallel ledgers or duplicate authority mechanisms if the current architecture already contains the necessary primitive.

Reuse the existing Grok Build adapter and Verified Change Workflow unless a concrete defect requires modification.

---

# G1 — Real operator-visible managed assignment

Provide one clear production entry point through which an operator can give a repository task to a Grok Build-backed Agent/Work item.

The assignment must contain, at minimum:

- repository identity
- immutable base revision
- bounded task/prompt
- allowed mutation scope
- execution approval state
- finite runtime/output/tool limits
- verification requirements

The resulting Work, Run, and Attempt must be observable through existing GrokPtah projections.

Do not build a second orchestration system.

---

# G2 — Truthful readiness before paid/provider dispatch

Before launching Grok Build, determine whether the host can actually perform the assignment.

Readiness must fail closed for missing or unsupported prerequisites including, as applicable:

- Grok Build executable
- supported CLI behavior
- usable repository/git
- supported host isolation
- credential/provider authority
- required verification tooling
- writable bounded candidate workspace
- configured limits

A readiness failure must result in:

- zero Grok Build provider dispatch
- zero ambiguous Run success
- durable explanatory state

Do not report READY merely because unit-test fixtures are available.

---

# G3 — Production credential containment

Resolve the currently unavailable production credential path sufficiently to permit a legitimate bounded live execution.

Requirements:

- GrokPtah remains the credential authority.
- The managed worker must not receive broader provider credentials than required.
- Credentials must not be persisted into the repository/worktree.
- Credentials must not appear in logs, evidence, prompts, Git objects, process diagnostics, or completion reports.
- Child execution must receive credentials through the narrowest currently practical lease/injection mechanism.
- The lease must have a bounded lifetime or equivalent revocation semantics.
- Restart/cancellation behavior must not silently convert an uncertain lease/process state into success.
- The implementation must document what revocation guarantees actually exist.

If a genuinely supportable production mechanism cannot be implemented because of an upstream Grok Build/xAI limitation, do not fabricate one.

Instead:

1. demonstrate the blocker concretely,
2. implement all safe surrounding machinery,
3. record the exact missing upstream capability,
4. leave the live gate BLOCKED.

But do not substitute another fake provider fixture and call G3 complete.

---

# G4 — One meaningful real repository change

Run the production managed-execution path against a disposable repository or purpose-built fixture repository with a **real software-engineering task**.

The task must:

- require changes to at least two source files, or equivalent meaningful implementation surface;
- have a deterministic failing behavior before the worker change;
- have a deterministic verification oracle;
- not be a README/version/DOGFOOD-only edit;
- require the worker to inspect code and implement a coherent fix;
- produce an inspectable Git candidate.

Prefer a small repository fixture specifically designed for this qualification rather than risking GrokPtah itself on the first live run.

The live worker may modify only its candidate workspace.

It must have no publication/merge authority.

---

# G5 — Candidate identity and independent verification

The result of the worker must become a retained candidate bound to:

- Work ID
- Run ID
- Attempt ID
- base revision
- exact candidate content identity
- mutation manifest
- required verification specification
- verification outcome

Required checks must be controlled by GrokPtah/the host, not merely asserted by the worker.

Where feasible, required check definitions/oracles must be outside the worker's writable scope.

Mutation after successful verification must invalidate or supersede that verification.

A worker message saying “tests passed” is not sufficient.

---

# G6 — Review and explicit disposition

After verification, the operator must be able to inspect:

- task
- base identity
- candidate identity
- files changed
- verification results
- relevant execution evidence
- attempt count
- stop reason
- provider/usage evidence where available

Passing verification must NOT automatically mutate the source checkout.

Support explicit disposition:

- approve/apply
- discard

If existing workflow supports additional states, preserve them.

Applying the candidate must use the existing Verified Change authority rather than adding a shortcut.

---

# G7 — Restart, cancellation, and ambiguity

Exercise actual durable boundaries.

At minimum prove:

1. restart before provider dispatch
2. restart after dispatch is recorded but before final result
3. restart with retained candidate awaiting verification/review
4. restart after verification but before disposition
5. cancellation before dispatch
6. cancellation while child execution exists
7. late child result after cancellation
8. repeated reopen after successful application
9. repeated reopen after discard

The system must not create duplicate paid dispatch merely because the service restarts.

Where process/provider state cannot be proven, represent that uncertainty explicitly.

Do not guess that a remote/provider action did or did not occur.

---

# G8 — Live qualification

If G3 establishes a real supported credential path, perform a bounded live provider-backed qualification.

Use:

- a disposable qualification repository
- minimal provider budget
- finite timeout
- one attempt
- no retry
- no Computer Use
- no publishing credentials
- no unrelated MCP/plugins/hooks
- explicit allowed mutation scope

Capture sanitized evidence sufficient to prove:

- readiness occurred before dispatch
- one provider-backed worker attempt occurred
- the intended repository/base was used
- candidate mutation remained bounded
- host verification ran independently
- result entered durable review state
- no secret appeared in retained evidence
- no source mutation happened before explicit disposition

Never print the credential while proving this.

If the live qualification cannot lawfully/safely run, mark it BLOCKED and state the exact blocker.

---

# G9 — Production-grounded regression coverage

Add regression coverage for the production path, including:

- readiness denial causes zero dispatch
- unsupported CLI/version fails closed
- credential acquisition failure causes zero dispatch
- candidate writes outside allowlist are rejected
- source repository remains unchanged during worker execution
- worker cannot modify host-owned verification authority
- candidate identity changes invalidate prior verification
- cancel prevents late success from authorizing application
- restart does not duplicate dispatch
- exact Work/Run/Attempt relationship survives reopen
- discard cannot later be mistaken for approval
- application still requires explicit Verified Change authority

Use real production components wherever practical.

Mocks may isolate external provider cost, but do not use mocks to prove properties that are specifically about the production adapter, filesystem, Git repository, or durable state.

---

# G10 — Operator evidence and independent-review handoff

Create:

`docs/goals/real-managed-assignment-v1-evidence.md`

The evidence document must include:

- frozen goal SHA
- base SHA/tree
- functional SHA/tree
- final published SHA/tree
- exact implementation branch
- draft PR
- architecture/path map
- commands executed
- test counts
- hosted workflow run IDs
- live qualification result, if run
- provider dispatch count
- candidate/base identities
- restart/cancel results
- known unavailable gates
- donor commits, if any, with explicit disposition
- deviations from the frozen goal
- remaining risks

Push all reviewable work normally.

Create/update a **draft PR**.

Do not merge it.

Independently verify that the remote branch points to the SHA claimed in the handoff.

---

# Explicit non-goals

Do NOT turn this into:

- a general multi-agent swarm
- Computer Use work
- browser qualification work
- scheduler redesign
- broad UI redesign
- new provider abstraction unrelated to the concrete need
- wholesale import of historical branches
- arbitrary platform sandbox expansion
- automatic merging
- automatic publishing
- hidden retry loops
- silent fallback to weaker security
- another fake DOGFOOD-only exercise

Keep the slice large enough to be product-significant but centered on **one genuine managed software-engineering assignment**.

---

# Historical branches

Old Claude/Codex/Cursor/Grok branches are donors only.

Do not merge a historical branch merely because it is ahead by commit count.

For any reused historical code:

- identify exact donor commit/path,
- compare it against current-main semantics,
- port only what is still required,
- record the disposition in the evidence document.

Current `main` is authoritative.

---

# Stop conditions

Stop and report BLOCKED rather than weakening the contract if:

- safe credential containment cannot be established,
- required host isolation cannot be established,
- the installed Grok Build interface cannot support the bounded execution,
- the production path would require giving the worker publication authority,
- a live attempt would expose provider secrets,
- exact candidate identity cannot be independently established.

---

# Completion report

Finish with exactly one overall status:

**COMPLETE**
**PARTIAL**
or
**BLOCKED**

Then report:

## Identity
Repository:
Branch:
Draft PR:
Frozen goal path:
Frozen goal commit:
Base SHA/tree:
Functional SHA/tree:
Final published SHA/tree:
Remote branch SHA verified:
Main unchanged during implementation:
Worktree clean:

## G1–G10
For each:
PASS | FAIL | BLOCKED

Give concise evidence.

## Real managed journey
Operator entry:
Work:
Run:
Attempt:
Executor:
Repository/base:
Candidate:
Verification:
Disposition:
Restart observations:
Cancellation observations:

## Provider qualification
Live provider run:
Credential mechanism:
Provider dispatch count:
Retry count:
Secret-retention check:
Known uncertainty:

## Validation
Exact commands:
Test counts:
Hosted workflow IDs:
Baseline failures:
New failures:
Skipped/ignored tests:

## Provenance
Donor commits:
Post-functional changes:
Evidence-only commits:
Local-only/unavailable artifacts:

## Remaining risks

## Independent-review focus

When the implementation and evidence are published, stop.

Do not merge, release, deploy, or begin another goal.

## Frozen starting identity and predecessor integration

Repository: `chriscase/GrokPtah`.

Implementation branch: `grok/real-managed-assignment-v1`.

Implementation base / integrated main: `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`; tree `f524b705a3b88cb895309506bf726e76c91874bf`.

PR #579 was verified and merged with a merge commit, `9e2660ff1a50edb7af1b81a4232d7fe23a37de0d`. Its parents preserve prior main `0dbe51c8aa94e3f26543be7a90e0085029487de7` and published predecessor tip `8451fb098a4b04d47110752fc37ea7b1e4ecb227`. Both that tip and independently reviewed functional revision `56757475fdfeba18247e936e16f240ecfa9f7f8f` are ancestors of the implementation base. The integrated tree equals the accepted published predecessor tree.

Pre-merge qualification was reverified: Desktop run `36258025951`, attempt 1, exact reviewed functional SHA, `pull_request`, success, no failed steps. The published head's Desktop check also succeeded (run `36260199676`). Post-merge smoke: `cargo fmt --check` passed; `cargo test --locked --test verified_change_workflow admission_resealed -- --test-threads=1` passed both tests using a disposable `GROKPTAH_HOME`.

The earlier prerequisite preflight made no implementation changes, created no implementation branch, and dispatched no provider request. Its BLOCKED state concerned the then-unintegrated predecessor only. Preflight provider dispatch count: **0**. Absence of an existing production lease is an implementation/research task under G3, not an automatic stop condition.

The G1–G10 criteria above are preserved unchanged in substance. This identity appendix records the verified starting state; it does not weaken any criterion. The goal is frozen by its first commit. The new PR must remain draft and unmerged for independent review.
