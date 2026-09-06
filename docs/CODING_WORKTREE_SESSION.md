# Coding Worktree Session v0 (Reversible code)

This document describes the durable Accept / Discard / Keep-for-review contract
for a disposable coding worktree added in this slice. It maps to **Windowed
Coding Run v0 must-have #3 (Reversible code)** and the Sep 18 / Day-30 product
trains. Computer Mode and `isolated_surface_admission_available()` remain **false**.

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
| Bridge integration tests | `grokptah-agent-bridge/tests/coding_worktree_session.rs` |

## Disposition contract

```text
Active → stage patch → Accept | Discard | KeepForReview → Settled
                              ↘ Uncertain (apply may have partially happened)
```

- **Accept** — apply the staged patch to an explicit `apply_target` only after an
  exact `sha256:` digest match. The protected main checkout is never an apply target.
- **Discard** — remove the disposable worktree and clear session records; the
  worktree must not remain registered or on disk.
- **Keep for review** — retain worktree + staged diff; no apply.
- **Uncertain** — recorded when apply may have partially happened (crash/restart
  mid-apply). No auto-retry and no auto-Accept on restart.

## Fail-closed invariants

| Invariant | Enforcement |
|---|---|
| Main checkout never written by session helpers | `assert_path_not_main_checkout` on write/accept |
| Accept requires exact patch digest | `PatchArtifact::verify_digest` |
| Discard cannot leave active worktree | `assert_worktree_removed` after `git worktree remove` |
| Restart/reload cannot auto-Accept | `recover_after_restart` → `Uncertain` when `apply_in_flight` |
| Uncertain apply → no auto-retry | `enforce_no_auto_retry` on all settlement ops |

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

- No UI for Accept / Discard / Keep-for-review disposition.
- No wiring into AgentHost session lifecycle or desktop chrome.
- No integration with isolated surface / Computer Mode admission.
- No live provider calls, host CGEvent, or TCC claims.
- Apply target selection is explicit API only; no automatic promotion to main.

## Non-claims

- Synthetic temp-dir harness success does **not** enable Computer Mode or isolated
  visual admission.
- This slice does **not** implement Virtualization.framework, Contained Browser, or
  the IsolatedSurfaceBackend SPI (parallel packet).
- Passing CI does **not** claim packaged VM / notarization / TCC qualification.
