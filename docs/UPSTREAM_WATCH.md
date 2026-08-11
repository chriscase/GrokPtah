# Upstream watch (Grok Build)

How GrokPtah tracks its parent, and the durable record of each audit cycle.

## Model: port, don't merge

GrokPtah has **no shared git history** with `xai-org/grok-build` (`git merge-base
origin/main upstream/main` is empty), and the parent's public repository is a
**squash mirror** — its history is a short series of `Synced from monorepo`
snapshots, each rewriting large parts of the tree. So:

- `git merge upstream/main` is not available and is not the intended workflow.
- There are no per-feature parent commits to cherry-pick.
- Per #108 / #144 / #179 and ADR-001, we **selectively port behaviour** into the
  thin bridge (`crates/codegen/grokptah-agent-bridge`) and the desktop layer.
  We do not wholesale re-vendor the parent crate tree.

Upstream commit *summaries* are leads only. Every finding below was checked
against parent **code and tests** and against the current bridge implementation.

## Current baseline

| | |
|---|---|
| Parent tip audited | `dd04f39397b1d02f2272b092555669dfba1f01bc85` (short `dd04f39`) |
| Parent `SOURCE_REV` | `2a28b4a86cfc4a4c133c35b7fc2a6a9964387c39` |
| Previous baseline | `3af4d5d` (cycle #185) |
| Range audited | `3af4d5d..dd04f39` — 9 sync snapshots |
| Fork commit at audit | `2e91663` |
| Audit issue | #208 |

Range shape: 1,088 files, +125,342 / −45,700 across `crates`. Churn is dominated
by `xai-grok-shell` (~60k) and `xai-grok-pager` (~45k, TUI presentation —
out of scope per #208), then `xai-grok-tools` (~18.6k).

Snapshots reviewed: `a5727c5`, `69f0ba8`, `6e38642`, `47348d1`, `b41c75a`,
`02d9359`, `5da6962`, `500129c`, `dd04f39`.

## Evidence matrix — cycle #208

Status is `present` (GrokPtah already does this), `adopt` (confirmed gap worth
porting), `defer` (real but not now, with reason), or `n/a` (out of scope).

| Area | Parent evidence | GrokPtah counterpart | Status | Sev | Verification |
|---|---|---|---|---|---|
| Process-tree kill + reap | `xai-tty-utils/process_scope.rs`; tests `wait_timeout_then_hard_kill_reaps_grandchild_tree`, `windows_job_kill_reaps_spawned_grandchild`, `scope_reaps_enrolled_language_server_child` | `bridge/src/process_tree.rs`: `process_group(0)`, `killpg(-pid, SIGKILL)`, Windows `taskkill /PID /T /F`, bounded `wait()` reap | **present** | — | Read `process_tree.rs` in full; group + tree + reap all covered cross-platform |
| Close/spawn race latch | `register()` now returns `bool` + latched `closed` flag, so a child enrolled after `kill_all` is killed on the spot instead of leaking a dead `Weak` | `live_shells` map + per-turn cancel token checked before dispatch + `kill_on_drop(true)` | **defer** | P2 | Narrow timing window only (cancel between the loop's cancel-check and the map insert). Mitigated but not equivalent: `kill_on_drop` kills the **direct child only**, not the descendant group, so a grandchild could survive that exact race |
| Repeated / no-op tool termination | `IdenticalToolCallRun`, `MAX_CONSECUTIVE_TRUE_NOOPS`, `command_is_true`; test `true_noops_chain_across_args_and_stop_at_4` | Only a blunt round cap: `max_agent_rounds` (default 24), `round_limit_stop_message` | **adopt** | P1 | `grep` for repetition/no-op detection in `host.rs` returns nothing relevant → filed **#209** |
| Cancel-all / per-child subagent cancel | subagent cancel work in range | `subagent_cancels: HashMap<String, CancellationToken>` — cancel one child without killing siblings; parent-linked (#151/#152) | **present** | — | `host.rs:116-117,1664-1697` |
| Prompt queue / steering races | queue + steering changes in range | `prompt_queues: HashMap<Uuid, SessionPromptQueue>`, non-cancelling steering inbox, `recover_pending_steering()`, durable via `load_all_prompt_queues` (#191) | **present** | — | `host.rs:132-135,172-175,254` |
| Honest task results | task-result work in range | `ToolCallStatus::{Completed,Failed}` recorded per dispatch | **present** | — | `host.rs:4177-4305` |
| `session/list`, streamed ACP tool calls | orchestration/ACP changes in range | Not in the bridge today; the embedded MCP orchestration control plane is **in flight** on PR #201 (`feat/200-standards-mcp-transport`, #196/#200) | **defer** | P2 | Deliberately not touched — filing or porting here would collide with active unmerged work |
| Extra-CA trust | new parent crate `xai-grok-extra-ca` | No custom-CA handling anywhere in the bridge | **defer** | P2 | Only matters behind a corporate TLS-intercepting proxy. Adjacent to #169/#170 (gateway/auth, closed). **Not verified**: whether #169's gateway path works behind a custom-CA proxy was not tested this cycle — flagged as honest residual debt, not asserted as broken |
| Model catalog | `default_models.json` changed by 1 line in range | Bridge catalog carries 8 ids incl. `grok-4.20-multi-agent` (#180) | **present** | — | GrokPtah is ahead of the parent file here, not behind |
| `8adf901` test-support wiring | `xai-grok-env` / `xai-grok-shell-base` `test-support` feature | Landed via #204, aligned to the parent implementation during review | **present** | — | Noted in #208 as the previously missed delta; merged |
| TUI presentation (`xai-grok-pager` ~45k, `xai-ratatui-textarea`) | large in range | Desktop GUI is not a TUI skin | **n/a** | — | Excluded by #208 scope |

## Honest residual debt

- **#209** (P1) is filed but **not implemented** in this cycle.
- The close/spawn race and extra-CA items are deferred with rationale above, not
  fixed, and no issue was filed (both P2; #208 asks for issues on P0/P1 only).
- ACP/`session/list` parity is unassessed on purpose while PR #201 is open; it
  should be re-checked in the next cycle once that work merges.
- Depth caveat: `xai-grok-shell` alone changed ~60k lines. This audit was
  **lead-driven** — it traced the specific behaviours #208 prioritised into
  parent code and tests, and checked each against the bridge. It is not a
  line-by-line reading of all 125k changed lines, and does not claim to be.

## Running the next watch

```sh
git fetch upstream
git log --oneline <baseline>..upstream/main            # snapshots since the pin
git diff --numstat <baseline>..upstream/main -- crates # rank churn by crate
```

Then, for each prioritised behaviour: find the parent implementation **and its
tests**, locate the GrokPtah counterpart, and record `present` / `adopt` /
`defer` / `n/a` with file:line evidence. Bump the baseline table above and note
what was left undone.

Next baseline to diff from: **`dd04f39`**.
