# #216 workspace persistence: atomic replacement evidence

Issue: [chriscase/GrokPtah#216](https://github.com/chriscase/GrokPtah/issues/216):
*desktop: chrome workspace.json atomic rename races under home override.*

| Field | Value |
|---|---|
| Writer | `GP-216-CLAUDE-20261107-R1` (sole source writer) |
| Base | `main` `7ddce0d5af8b7d4ac8a3e27e839ab6a232ab141e`, tree `ce88b95730e03ce7dfc499efb17d50ed5d69aab2` |
| Branch | local `claude/grokptah-216-workspace-atomic-save` (not pushed) |
| Product paths | `crates/codegen/grokptah-agent-bridge/src/session_store.rs` (private helper, local imports, focused tests); this file |
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
3. B's `rename` fails with `ENOENT`: its staging file is gone. This is the soak
   residual reported in #216.

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
  the cleanup failure is added as context and the original failure stays the
  root cause. Other writers' staging files and the destination are never
  removed.
- **Ordering.** Concurrent successes leave exactly one complete submitted
  snapshot: whichever rename commits last. No start-order guarantee is claimed.
- **Test seam.** A `#[cfg(test)]` thread-local seam (`staging_seam`) provides
  two hook points, `BeforeWrite` and `BeforeRename`, plus forced staging
  suffixes. It is compiled out of non-test builds and cannot affect any thread
  except the one that installed it.

## Focused regressions (authored, not executed)

All of these are in `session_store::tests`. Each runs under an isolated project
home (`IsolatedHome`: `home_override_serial` plus a temporary directory) whose
drop restores the override before removing the directory. Tests that start
workers use `std::thread::scope`, so every worker is joined first, including on
panic. No real home, global settings, sleeps or new dependencies are involved.

| Test | Covers | Seam |
|---|---|---|
| `save_chrome_leaves_another_writers_staged_snapshot_alone` | **Parent-red.** Another writer's staged file is not truncated or moved, and its rename still succeeds (the base fails with ENOENT). Reopen. | none |
| `a_writer_holding_the_old_shared_staging_name_cannot_tear_published_chrome` | **Parent-red.** A writer still holding the shared name cannot tear the published chrome (the base publishes `B + tail of A`). Reopen. | none |
| `interleaved_save_chrome_writers_stage_privately_and_both_commit` | Two real `save_chrome` writers, deterministically interleaved: distinct same-directory staging files, no truncation, both succeed, commit-order survivor, no leftovers, reopen. | `BeforeRename` park |
| `concurrent_save_chrome_writers_all_succeed_and_publish_a_submitted_snapshot` | 4 writers × 8 rounds behind a barrier: all succeed, the published file is one submitted snapshot, no ordering assumed. | none |
| `save_chrome_overwrite_replaces_the_whole_previous_snapshot` | A short overwrite leaves no tail of the longer predecessor. Reopen. | none |
| `serialization_failure_creates_nothing_and_preserves_the_destination` | A real encoder error; no staging file; destination and foreign staging byte-identical. | none |
| `write_failure_is_reported_and_removes_only_its_own_staging_file` | A write failure is reported with its cause; only the write's own staging file is removed; foreign staging and destination intact. | `BeforeWrite` fault |
| `real_rename_failure_is_reported_and_removes_only_its_own_staging_file` | A real OS rename refusal (a directory occupies the destination): the OS error is the cause; the occupant and foreign staging are intact. | observe only |
| `cleanup_failure_keeps_the_original_error_and_never_removes_a_tree` | A failed unlink keeps the original root cause; cleanup never removes a directory tree. | `BeforeRename` fault |
| `an_existing_staging_name_is_skipped_untouched` | A colliding name is skipped without being opened or modified. | forced suffix |
| `exhausted_staging_names_fail_without_touching_anything` | Bounded attempts; every taken name and the destination are unchanged. | forced suffixes |
| `v1_migration_still_splits_sessions_and_rewrites_chrome` | v1 migration: chrome fields, byte-exact `.v1.bak`, per-session meta and transcript, no staging leftovers. | none |
| `prompt_queue_and_session_meta_still_round_trip` | Queue order, queue overwrite, and meta reload through the helper. | none |
| `durable_writers_still_demand_write_authority` | A compile-time pin: `save_chrome`, `save_session_meta` and `save_prompt_queue` still require `&DurableWriteGuard`. | none |

"Parent-red" is a claim from reading the source: the first two tests use only
APIs that exist on the base, and their assertions contradict the base
behaviour described above. It has **not** been demonstrated by execution.

## Required span outside this assignment

Changing the helper stops the existing fault injectors of **three tests outside
the two allowed paths** from working. Each one plants a directory at the old
shared staging name `sessions/<id>/prompt_queue.json.tmp` so that `File::create`
fails. With private staging names that directory no longer blocks the write, so
these tests will fail until their injector is replaced:

- `crates/codegen/grokptah-agent-bridge/src/host.rs`: `block_queue_persistence`
  (around line 12857), used by
  `a_failed_recovery_write_keeps_the_steering_and_reports_the_failure` and
  `the_abort_path_reports_a_failed_recovery_write_too`.
- `crates/codegen/grokptah-agent-bridge/tests/orchestration_adversarial.rs`:
  `queue_persistence_failure_does_not_mutate_memory` (around line 1050).

Suggested equivalent injector, for whoever owns that span: plant the directory
at the **destination** `sessions/<id>/prompt_queue.json` instead. Then the real
`rename` fails on both the base and this helper. These tests read the queue
from memory (`session_queue_list` uses the in-memory snapshot; disk is read
only at host boot), so the change should not otherwise alter what they observe.
It has not been run. These files were not edited here; the change is returned
to the coordinator for a span decision.

## Out-of-scope residuals (not changed)

- The same shared-staging pattern exists outside the assigned helper:
  `save_session_subagents` (`subagents.json.tmp`, inline `fs::write` + rename),
  `rewrite_transcript` (`transcript.jsonl.tmp`), `orchestration/store.rs`
  `atomic_write_json`/`atomic_write_bytes`, `computer_use/store.rs`,
  `gateway_config.rs`, and `xai-host-authority` `store.rs`.
- A crash between staging and rename now leaves an orphaned
  `.<name>.tmp-<uuid>`. Before this change the next write reused the shared
  name. By contract, cleanup removes only the failing operation's own file, so
  no sweep was added. A legacy `*.json.tmp` left by an older build is likewise
  left alone.

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
   warnings`, and `cargo test --locked -- --test-threads=1`. These need the
   cross-span injector change first.
3. Supported Desktop Rust tests (`desktop/src-tauri` `cargo test --locked`), and
   frontend `npm ci && npm run typecheck && npm test`.
4. Exact-head hosted Desktop CI.
5. An independent adversarial review of concurrency, failure handling, privacy
   and write authority by a reviewer other than the author.

Formatting and lint conformance were checked by hand against `rustfmt.toml`,
`clippy.toml` and the default clippy lint set. Treat that as review, not as a
gate result.
