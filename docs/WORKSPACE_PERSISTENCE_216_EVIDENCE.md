# #216 workspace persistence: atomic replacement evidence

Issue: [chriscase/GrokPtah#216](https://github.com/chriscase/GrokPtah/issues/216):
*desktop: chrome workspace.json atomic rename races under home override.*

| Field | Value |
|---|---|
| Writer | `GP-216-CLAUDE-20261107-R1` (sole source writer); amendment `GP-216-CLAUDE-20261107-R1-A1`, same writer |
| Base | `main` `7ddce0d5af8b7d4ac8a3e27e839ab6a232ab141e`, tree `ce88b95730e03ce7dfc499efb17d50ed5d69aab2` |
| Branch | local `claude/grokptah-216-workspace-atomic-save`, pushed (no force) to the assigned remote branch `claude/great-clarke-gxc4hm`; no PR |
| Product paths | `crates/codegen/grokptah-agent-bridge/src/session_store.rs` (private helper, local imports, focused tests); this file |
| A1 test-only paths | `crates/codegen/grokptah-agent-bridge/src/host.rs` (test fixture only); `crates/codegen/grokptah-agent-bridge/tests/orchestration_adversarial.rs` (one test's fixture only) |
| Execution | **UNRUN**: see [Execution status](#execution-status) |

The exact head, tree, diff and bundle for a handoff are recorded in the
handoff manifest, not here, because this file is part of that tree.

## Parent mechanism (source-confirmed)

`save_chrome`, `save_session_meta` and `save_prompt_queue` all persist through
the private `atomic_write_json`. On the base it staged every write at one fixed
name, `path.with_extension("json.tmp")`, opened with `File::create` (truncate)
and then renamed over the destination. `persist_chrome` runs from many host
methods under a counted, non-exclusive `DurableWriteGuard`, so two writes to the
same destination can overlap:

1. Writer B creates `workspace.json.tmp` and writes its snapshot.
2. Writer A's `File::create` reopens **the same file** and truncates it, writes
   A's bytes, and renames it to `workspace.json`.
3. B's `rename` fails with `ENOENT`: its staging file is gone. This produces
   the `rename … ENOENT` symptom reported in #216.

The issue does not record which trigger the soak run hit. This race is a
confirmed mechanism for that symptom, but it may not be the only one. A write
still in flight when a test's temporary home is removed, or when the home
override is restored underneath it, also ends in `ENOENT`. This change does not
address that path. Such a write still fails with an honest `ENOENT`. A soak
re-run therefore stays an open gate (see the end of this document).

A second interleaving corrupts the published file. If B opened the shared name
before A published it but writes after, B's bytes land in the inode that is now
`workspace.json`. A shorter B leaves `B + tail of A`, which is unparseable JSON,
and `load_workspace` silently falls back to default chrome on the next start.

## Change

Only the private replacement helper changed. Public signatures, the store
schema/version, the v1 migration, session metadata, transcript and prompt-queue
behaviour, and every `DurableWriteGuard` requirement are untouched.

- **Distinct, exclusively created staging file per write.** The name is
  `.<name>.tmp-<uuid v4>` in the destination's own directory, so the rename
  stays on one filesystem. It is opened with `create_new`, so an existing file
  is never opened, truncated or reused. This mirrors the repository's existing
  idiom in `orchestration/store.rs` `write_json_exclusive`. On `AlreadyExists`
  a fresh name is drawn, up to 8 attempts, then the write fails.
- **Encode first.** A serialization failure returns before any file is
  created.
- **Atomic replacement and durability unchanged.** The write, flush,
  best-effort `sync_all`, close, best-effort parent sync, `rename` and
  best-effort parent sync happen in the same order as before. Platforms without
  full fsync still get an atomic rename.
- **Honest failure, minimal cleanup.** Serialization, create, write, flush and
  rename failures are returned with their cause as the error's source. On
  failure the helper unlinks **only** the staging path this operation created
  (`remove_file`, never a tree), ignoring `NotFound`. If that unlink fails too,
  the original failure stays the root cause. The cleanup failure is added as
  outer context that starts with the original message, so callers that print
  only `{error}` (for example `persist steering recovery: {error}` in
  `host.rs`) still see the original failure first. Other writers' staging
  files and the destination are never removed.
- **Ordering.** Concurrent successes leave exactly one complete submitted
  snapshot: whichever rename commits last. No start-order guarantee is claimed.
- **Test seam.** A `#[cfg(test)]` thread-local seam (`staging_seam`) provides
  two hook points, `BeforeWrite` and `BeforeRename`, plus forced staging
  suffixes. It is compiled out of non-test builds and cannot affect any thread
  except the one that installed it.

## Focused regressions (authored, not executed)

All of these are in `session_store::tests`. Every test that touches the
filesystem runs under an isolated project home (`IsolatedHome`:
`home_override_serial` plus a temporary directory). Its drop restores the
override before removing the directory. Tests that start workers use
`std::thread::scope`, so every worker is joined first, including on panic.

The concurrent test starts its writers behind an `RwLock` gate that the main
thread releases once all writers are spawned. If a spawn panics, unwinding
drops the gate and the writers already started can still finish, so the join
cannot hang. No real home, global settings, sleeps or new dependencies are
involved.

| Test | Covers | Seam |
|---|---|---|
| `save_chrome_leaves_another_writers_staged_snapshot_alone` | **Parent-red.** Another writer's staged file is neither truncated nor moved. Its own rename still succeeds and publishes its exact bytes; on the base, this `expect` fails with the issue's `No such file or directory`. Reopen. | none |
| `a_writer_holding_the_old_shared_staging_name_cannot_tear_published_chrome` | **Parent-red.** A writer still holding the shared name cannot tear the published chrome (the base publishes `B + tail of A`). Reopen. | none |
| `interleaved_save_chrome_writers_stage_privately_and_both_commit` | Two real `save_chrome` writers, deterministically interleaved: distinct same-directory staging files, no truncation, both succeed, commit-order survivor, no leftovers, reopen. | `BeforeRename` park |
| `concurrent_save_chrome_writers_all_succeed_and_publish_a_submitted_snapshot` | 4 writers × 8 rounds behind a start gate: all succeed, the published file is one submitted snapshot, no ordering assumed. | none |
| `save_chrome_overwrite_replaces_the_whole_previous_snapshot` | A short overwrite leaves no tail of the longer predecessor. Reopen. | none |
| `serialization_failure_creates_nothing_and_preserves_the_destination` | A real encoder error; no staging file; destination and foreign staging byte-identical. | none |
| `write_failure_is_reported_and_removes_only_its_own_staging_file` | A write failure is reported with its cause; only the write's own staging file is removed; foreign staging and destination intact. The fault is injected just before `write_all`. A real `write_all` or `flush` error goes through the same `?` path but is not exercised. | `BeforeWrite` fault |
| `real_rename_failure_is_reported_and_removes_only_its_own_staging_file` | A real OS rename refusal (a directory occupies the destination): the OS error is the cause; the occupant and foreign staging are intact. | observe only |
| `cleanup_failure_keeps_the_original_error_and_never_removes_a_tree` | A failed unlink keeps the original root cause, and the outermost message still starts with the original failure. Cleanup never removes a directory tree. | `BeforeRename` fault |
| `an_existing_staging_name_is_skipped_untouched` | A colliding name is skipped without being opened or modified. | forced suffix |
| `exhausted_staging_names_fail_without_touching_anything` | Bounded attempts; every taken name and the destination are unchanged. | forced suffixes |
| `v1_migration_still_splits_sessions_and_rewrites_chrome` | v1 migration: chrome fields, byte-exact `.v1.bak`, per-session meta and transcript, no staging leftovers. | none |
| `prompt_queue_and_session_meta_still_round_trip` | Queue order, queue overwrite, and meta reload through the helper. | none |
| `durable_writers_still_demand_write_authority` | A compile-time pin: `save_chrome`, `save_session_meta` and `save_prompt_queue` still require `&DurableWriteGuard`. | none |

"Parent-red" is a claim from reading the source: the first two tests use only
APIs that exist on the base, and their assertions contradict the base
behaviour described above. It has **not** been demonstrated by execution.

**Running the parent-red tests on the base.** The rest of this test module uses
`staging_seam` and `STAGING_ATTEMPTS`, which do not exist on the base, so the
module as a whole will not compile there. To run the parent-red check:

1. Start from the base `session_store.rs` (`7ddce0d5`).
2. Append these items from this change's `tests` module, and nothing else:
   `IsolatedHome` (struct and both impls), `write_authority`, `chrome`,
   `snapshot`, `read_bytes`, `staging_names`,
   `save_chrome_leaves_another_writers_staged_snapshot_alone` and
   `a_writer_holding_the_old_shared_staging_name_cannot_tear_published_chrome`.
   They need no import beyond the base module's.
3. Run only those two tests.

Expected result on the base:

- The first test fails at writer B's rename with `No such file or directory`.
- The second fails because the published bytes are not A's snapshot.

The same two tests must then pass unchanged on this head.

## A1: test-only fault-injector replacement

**Need.** The helper change disabled the fault injector of three existing tests
outside the original two paths. Each planted a directory at the old shared
staging name `sessions/<id>/prompt_queue.json.tmp` so that `File::create`
failed. With private staging names that directory no longer blocks anything.

**Authorization.** The coordinator bound amendment
`GP-216-CLAUDE-20261107-R1-A1` to the same writer on the unchanged base. Its
scope is test-only:

- `src/host.rs`: `tests::block_queue_persistence` and its explanatory comment
  (base line 12854), plus only the fixture setup and cleanup needed in
  `a_failed_recovery_write_keeps_the_steering_and_reports_the_failure` and
  `the_abort_path_reports_a_failed_recovery_write_too`.
- `tests/orchestration_adversarial.rs`: only the failure fixture and the cleanup
  needed in `queue_persistence_failure_does_not_mutate_memory` (base line 1047).

**Replacement.** The new injector is a genuine filesystem failure that does not
depend on any staging name: an owned, non-empty directory at the **final**
`sessions/<id>/prompt_queue.json`. The store's real `rename` over it fails on
both the base helper and this one (`EISDIR` on Linux and macOS; refused on
Windows). Each fixture:

1. Asserts the destination does not exist yet. `session_new_kind` writes only
   meta, transcript and chrome; queues reach disk only on a queue mutation or
   an ordered-shutdown flush.
2. Creates the directory and writes a sentinel file inside it.
3. After the behavioural assertions, asserts the sentinel is byte-identical,
   then removes only the sentinel and the directory. `remove_dir` is
   non-recursive, so it refuses to delete a directory that holds anything
   else. This happens before host teardown.

In `host.rs`, `block_queue_persistence` now returns a `QueueObstacle` guard.
`verify_and_remove` performs step 3 and surfaces any failure. On a panic path,
`Drop` performs the same non-recursive removal best-effort. The guard is bound
after the host, so it is dropped before the host. Implicit `HostRuntime` drop
seals durable writes and quarantines the lock, but does not flush prompt
queues.

**Unchanged.** No production code, fault switch, permission change, transcript
change, API change, guard change or queue change. Every behavioural assertion
is kept:

- A failed steering recovery keeps the live steering and reports
  `persist steering recovery`.
- It publishes no authoritative `PromptQueueChanged`.
- The abort path reports the same failure.
- A failed queue addition leaves memory unchanged.

The queue is read from memory in all three tests (`session_queue_list` uses the
in-memory snapshot; disk is read only at host boot).

**Owner reconciliation, as recorded by the coordinator:**

- #585 changes neither file.
- The recorded #586 head `66157216` patches `host.rs` only near base line 2640
  and excludes `orchestration_adversarial.rs`. Every A1 hunk in `host.rs` is in
  the `tests` module from base line 12853 onward.
- The historical #417 host tests, #474 auth arguments and assertions, and the
  published #468 test hunks are not touched.
- No production span, draft or lock of those owners is released.

A1 is UNRUN like everything else here. The Rust 1.92 toolchain is unavailable,
and no 1.97 substitute was used.

## Out-of-scope residuals (not changed)

- The same shared-staging pattern exists outside the assigned helper:
  `save_session_subagents` (`subagents.json.tmp`, inline `fs::write` + rename),
  `rewrite_transcript` (`transcript.jsonl.tmp`), `orchestration/store.rs`
  `atomic_write_json`/`atomic_write_bytes`, `computer_use/store.rs`,
  `gateway_config.rs`, and `xai-host-authority` `store.rs`.
- A crash between staging and rename now leaves an orphaned
  `.<name>.tmp-<uuid>`. Before this change the next write reused the shared
  name. By contract, cleanup removes only the failing operation's own file, so
  no sweep was added. Repeated crashes can therefore accumulate orphans without
  bound. An owner-scoped startup sweep, run under the instance lock, would be
  a separate follow-up. A legacy `*.json.tmp` left by an older build is
  likewise left alone.
- `src/host.rs` lines 666–667 still justify skipping a second recovery write
  as avoiding "contention on the session's temp file". With private staging
  files there is no shared temp file to contend on. The reasoning (the write is
  already durable) still holds, but the wording is stale. The comment is
  production code outside the A1 test-only scope, so it was left as is.
- Windows was not analysed beyond std's documented rename-over-existing
  behaviour. Whether many concurrent replace-renames onto one destination can
  return a sporadic access-denied error depends on std's Windows rename path.
  Bridge CI runs on macOS only.

## Independent review

**Provenance.** R1 to R3 below were run by one **in-session background
subagent**: a read-only Claude Opus agent with no author context, started from
the author's session. It is independent of the author's reasoning but it is
**not a distinct reviewer session**, so it does not satisfy the
distinct-session review gate. That gate needs a separate session reviewing the
exact frozen head, recorded in the handoff manifest. None of these verdicts
transfers to any later head.

**R1:** the background subagent reviewed source only (no execution) at head
`70d5eac9`. Verdict: **BLOCKING**, solely on F1.

| Finding | Severity | Disposition |
|---|---|---|
| F1: three out-of-scope tests lose their fault injector | blocking | Fixed by A1 (`1759f6f4`). |
| F2: test 1 failed on the base before reaching the ENOENT rename | should-fix | B's rename now runs before the content check, so the base fails with the real ENOENT. |
| F3: the parent-red procedure on the base was not stated | should-fix | Transplant procedure added above. |
| F4: the root cause was asserted, not shown | should-fix | Reworded; soak re-run kept as an open gate. |
| F5: on cleanup failure, `{error}` callers lost the original message | nit | The outer context now starts with the original message; the test checks it. |
| F6: a real `write_all`/`flush` error is not exercised | nit | Documented in the test table. |
| F7: not every test runs under an isolated home | nit | Wording corrected. |
| F8: orphan accumulation | nit | Documented as a residual and a follow-up. |
| F9: barrier hang if a spawn panics | nit | Replaced with an `RwLock` start gate released by unwinding. |
| F10: Windows concurrent renames | uncertain | Documented as a residual. |
| Stale comment `host.rs:666-667` | nit, out of scope | Documented as a residual. |

**R2:** the same background subagent reviewed source only at head `66bec50e` (A1 plus the
R1 corrections). Verdict: **BLOCKING**, solely on N1. It found A1 and every R1
correction otherwise resolved.

| Finding | Severity | Disposition |
|---|---|---|
| N1: `QueueObstacle { dir, removed: false }` exceeds rustfmt's default `struct_lit_width` (18) | blocking (fmt) | Reformatted vertically. |
| N2: the integration-test obstacle is removed only on the success path | nit | Left as is. On a panic, implicit `HostRuntime` drop does not flush queues, and `TempDir` removes the tree. A guard would widen the authorized fixture. |
| N3: the A1 tests do not pin the failure reason to the rename | nit | Left as is. Pinning it would change behavioural assertions beyond the fixture-only scope; the reason is right by construction. |
| N4: with `{error:#}`, the cleanup note repeats the original top message | cosmetic | Accepted. The repetition is the price of keeping `{error}` readers correct. |
| N5: A1 adds fixture items (`QUEUE_OBSTACLE_SENTINEL`, `QueueObstacle`) inside `host.rs` `mod tests` | scope observation | Every item is part of the `block_queue_persistence` fixture; reported to the coordinator. |
| N6: the owner-reconciliation claims cannot be verified locally | cannot verify | These are coordinator records and are labelled as such. |

**R3:** the same background subagent reviewed source only at `4368e89d`.
Verdict: **NON-BLOCKING**. N1 is resolved, the R2 table matches its report, and
the N2 to N6 items and the documented residuals are open but not blocking.
This head differs from `4368e89d` only by this provenance correction, so R3
does not carry over to it.

**D1, distinct reviewer session.** A separate cloud session reviewed the
frozen head `862d91e4` (tree `bf8f54be`), source only and read-only. Verdict:
**BLOCKING**, solely on its F1. It found every acceptance item and A1 met by
source tracing. The full report is in the handoff packet.

| D1 finding | Severity | Disposition |
|---|---|---|
| F1: two one-line `staged_rx.recv().expect(..)` chains in the write-failure and cleanup-failure tests are 61 characters, over rustfmt's `chain_width` (60) | blocking (fmt) | Both chains split vertically. A mechanical scan of every added Rust line found no other chain of 59 or more. It also found no struct-literal body over 16 and no comma-separated one-line argument list over the limit. The two sole-closure arguments wider than 60 match an overflow layout that the crate's fmt-clean base code uses 568 times. |
| F2: the doc's branch row said "not pushed" | nit | Updated: pushed without force to `claude/great-clarke-gxc4hm`, no PR. |
| F3: the A1 initial-state check treated any error as "absent" | nit | Both fixtures now require `ErrorKind::NotFound` exactly. |
| F4: the integration-test obstacle is cleaned up only on success | nit | Unchanged; already documented (R2 N2). |
| F5: Windows concurrent renames | uncertain | Unchanged; documented residual. |
| F6: other stores still use a fixed shared `.tmp` | out of scope | Unchanged; documented residual. |

D1 does not carry over to the corrected head. That head needs its own
distinct-session review, recorded in the handoff manifest.

## Execution status

**UNRUN.** No test, build, format, lint or version command was executed. The
executable resolution was inspected read-only:

- `cargo`, `rustc`, `rustfmt`, `cargo-clippy` and `clippy-driver` on `PATH` are
  rustup proxies (`/root/.cargo/bin/* → rustup`).
- The repository pins `rust-toolchain.toml` `channel = "1.92.0"`. The only
  installed toolchain is `stable-x86_64-unknown-linux-gnu` = `1.97.0
  (2d8144b78 2026-07-07)`, and `RUSTUP_AUTO_INSTALL` is unset. Any proxy
  invocation inside the repository would download 1.92.0, which is implicit
  acquisition. Calling the 1.97.0 binaries directly would bypass the frozen
  compiler pin.
- There is no `~/.cargo/registry` or `~/.cargo/git` and no `target/` cache, so
  `cargo … --locked` would download every crate in the bridge `Cargo.lock`.
  `--offline` cannot succeed.
- There is no `desktop/node_modules`, so the frontend gates need `npm ci`
  (network).
- No fresh, finite, one-worker resource admission was issued.

Missing prerequisites for execution: the installed 1.92.0 toolchain with
rustfmt and clippy, with known provenance; a pre-populated, provenance-checked
Cargo cache for `crates/codegen/grokptah-agent-bridge/Cargo.lock`, with network
disabled; an owned isolated home/config profile; and an explicit resource
admission.

## Remaining whole-issue gates (none run)

1. The focused `session_store` regressions above, including an executed
   parent-red run of the first two tests against the base helper.
2. Bridge `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D
   warnings`, and `cargo test --locked -- --test-threads=1`. These include the
   two A1 `host.rs` fixture tests, the A1 integration test and the
   `session_store` tests.
3. Supported Desktop Rust tests (`desktop/src-tauri` `cargo test --locked`), and
   frontend `npm ci && npm run typecheck && npm test`.
4. Exact-head hosted Desktop CI.
5. A soak re-run under the home override, to confirm the residual reported in
   #216 is gone and not caused by another mechanism (see
   [Parent mechanism](#parent-mechanism-source-confirmed)).
6. An independent adversarial review of concurrency, failure handling, privacy
   and write authority, in a **distinct reviewer session** on the exact frozen
   final head.

Formatting and lint conformance were checked by hand against `rustfmt.toml`,
`clippy.toml` and the default clippy lint set. Treat that as review, not as a
gate result.
