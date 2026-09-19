# Coding Worktree Session v0 (Reversible code)

This document describes the durable Accept / Discard / Keep-for-review contract
plus local Pause / fence-first Stop for a disposable coding worktree. It maps to
**Windowed Coding Run v0 must-have #3 (Reversible code)** and the Sep 18 / Day-30
product trains. Computer Mode and `isolated_surface_admission_available()` remain
**false**.

Related issues: [#288](https://github.com/chriscase/GrokPtah/issues/288) (isolated
visual), [#286](https://github.com/chriscase/GrokPtah/issues/286) (agent-owned
surface), [#267](https://github.com/chriscase/GrokPtah/issues/267) (epic).

## What this slice adds

| Deliverable | Location |
|---|---|
| `CodingWorktreeSession` contract | `crates/codegen/grokptah-coding-worktree/src/session.rs` |
| Patch artifact + digest | `grokptah-coding-worktree/src/patch.rs` |
| Session lifecycle + disposition | `grokptah-coding-worktree/src/lifecycle.rs` |
| Git worktree helpers (scoped writes) | `grokptah-coding-worktree/src/git.rs` |
| Restart snapshot for recovery tests | `grokptah-coding-worktree/src/store.rs` |
| Invariant regression suite | `grokptah-coding-worktree/tests/session_invariants.rs` |
| Bridge fail-closed seam | `grokptah-agent-bridge/src/coding_worktree.rs` |
| AgentHost disposition surface | `AgentHostHandle::coding_worktree_*` in `grokptah-agent-bridge/src/coding_worktree.rs` |
| Bridge integration tests | `grokptah-agent-bridge/tests/coding_worktree_session.rs` |

## Disposition contract

```text
Active → Pause (fences staging/settlement; Stop remains legal)
Active → stage patch → Accept | Discard | KeepForReview → Settled
                              ↘ Uncertain (apply may have partially happened; no auto-retry)
Active | Paused → Stop (fence-first teardown)
                  ↘ Destroyed only if worktree destroy is confirmed
                  ↘ Stopped if destroy fails (never claim Destroyed)
```

- **Accept** — apply the staged patch to an explicit `apply_target` only after an
  exact `sha256:` digest match. The protected main checkout is never an apply target.
- **Discard** — remove the disposable worktree and clear session records; the
  worktree must not remain registered or on disk.
- **Keep for review** — retain worktree + staged diff; no apply.
  Attach/restore after Accept, Discard, or Keep-for-review keeps that disposition
  and cannot re-enter staging, settlement, or Pause.
- **Uncertain** — recorded when apply may have partially happened (crash/restart
  mid-apply). No auto-retry and no auto-Accept on restart.
- **Pause** — local authority fence; further staging and settlement are rejected.
  Stop remains legal. Not Computer Mode.
- **Stop** — fence-first teardown. Persist/snapshot failure does not skip destroy.
  `Destroyed` is recorded only when worktree destroy is confirmed.

## Fail-closed invariants

| Invariant | Enforcement |
|---|---|
| Main checkout never written by session helpers | `resolve_worktree_write_path` canonicalizes and confines writes to disposable worktree root; rejects absolute paths and `..` escapes |
| Accept targets outside main checkout prefix | `assert_apply_target_allowed` rejects any path equal to or under protected checkout |
| Accept requires exact patch digest of bytes applied | `PatchArtifact::verify_digest` hashes `bytes`; `apply_patch` re-hashes before `git apply` |
| Discard cannot leave active worktree | `git worktree remove` + `prune` + `git worktree list` must not list session path |
| Staging includes untracked files | `git add -N` intent-to-add before diff capture; fail-closed if untracked present but diff empty |
| Restart/reload cannot auto-Accept | `recover_after_restart` → `Uncertain` when `apply_in_flight`; crate `attach` and host `coding_worktree_attach` invoke it |
| Uncertain apply → no auto-retry | `enforce_no_auto_retry` on settlement ops and Pause |
| Attach after settlement cannot retry | `reconcile_invariants` fences Accepted/Discarded/KeptForReview; Pause and settlement stay closed |
| Pause fences further work | `begin_pause` sets `pause_fenced`; staging/settlement reject |
| Stop never claims Destroyed without confirmed destroy | `complete_destroy` only when worktree is gone; else `Stopped` |
| Persist failure does not skip teardown | `stop` records persist errors and still attempts destroy |

## Test harness policy

All CI tests use **synthetic temp-dir git fixtures** under `tempfile::TempDir`. They
do **not** read or write the developer's real GrokPtah checkout. See
`SYNTHETIC_SESSION_NONCLAIM` in the crate.

## Verification commands

```sh
# Format + lint (agent-bridge workspace)
cargo fmt --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all
cargo clippy --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --all-targets -- -D warnings

# Focused harness tests
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test coding_worktree_session -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-coding-worktree/Cargo.toml \
  --test session_invariants -- --test-threads=1
```

## Residuals (honest, post-slice)

Closed by the AgentHost disposition surface (`coding_worktree_*` on
`AgentHostHandle`):

- Host owns/attaches one `CodingWorktreeSession` per explicit handle (optionally
  bound to an AgentHost session) and exposes Pause, fence-first Stop, Accept
  (exact `sha256:` digest, never protected main / host project cwd / bound
  session cwd), Discard, and Keep-for-review. Mutators take `#455` durable-write
  authority; Stop still tears down if persist/write authority fails.
- Pause still fences staging/settlement; attach-after-settlement cannot retry;
  Uncertain rejects auto-retry; crate `attach` and host `coding_worktree_attach`
  recover apply-in-flight (including hostile Active+in-flight) to Uncertain;
  Stop remains legal while Paused;
  `Destroyed` is recorded only on confirmed destroy. Deleting a bound AgentHost
  session fence-first Stops its coding worktree and refuses unconfirmed destroy.
  Archive Pauses a bound worktree. Host `stop` fence-first Stops attached
  coding worktrees.
- Bridge tests in `coding_worktree_session.rs` cover those host paths.

Still open:

- No UI / Tauri command wrappers for Accept / Discard / Keep-for-review /
  Pause / Stop. Desktop chrome is a follow-up; this slice is host+tests only.
- No integration with isolated surface / Computer Mode admission.
- No live provider calls, host CGEvent, or TCC claims.
- Apply target selection is explicit API only; no automatic promotion to main.

## Non-claims

- Synthetic temp-dir harness success does **not** enable Computer Mode or isolated
  visual admission.
- This slice does **not** implement Virtualization.framework, Contained Browser, or
  the IsolatedSurfaceBackend SPI (parallel packet).
- Passing CI does **not** claim packaged VM / notarization / TCC qualification.
