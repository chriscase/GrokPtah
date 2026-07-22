# Parity scoreboard (live) — discriminating suite

**Program:** Make the parity eval tell the truth  
**Harness:** `evals/scripts/run_parity.sh` + bridge `examples/live_eval.rs` + structured `eval_oracle`  
**Related:** continuous discriminating cycle (post-#185)

## Definition of “≥ Grok Build”

On the fixture set in `evals/tasks.json`, GrokPtah is ≥ Grok Build CLI when:

1. **Success rate** ≥ CLI success rate, and  
2. If equal, **tool_errors** (sum) ≤ CLI,

with both sides live, **same model**, YOLO / always-approve, **two consistent runs**.

**Offline smokes and uniform 100% sweeps are not a capability proof.**  
Hard tasks use **structured oracles** (`command` / `exact_file` / `all` composites) — not prose-substring-only predicates.

## Suite composition

| Task id | Diff | Difficulty rationale |
|---------|------|----------------------|
| basic_edit_add_mul | smoke | Trivial API add |
| basic_edit_readme_token | smoke | Exact artifact (`exact_file`) |
| ambiguous_rank_order | hard | Conflicting PRODUCT vs SPEC docs |
| cross_cut_legacy_widget | hard | Multi-file rename + re-exports under tight max_turns |
| recover_wrong_notes | hard | Recover after wrong prior attempt |
| hist_utf8_truncate | hard | Fork bug class #115 / 9c1426a |
| hist_always_allow_scope | hard | Fork bug class #110 / 39a8fd3 |
| hist_jsonl_torn_line | hard | Fork session_store torn JSONL |
| long_horizon_trap_bug99 | hard | README trap vs BUG99 authoritative |
| lru_cache_impl | hard | LRU eviction + update-touch edges |
| rename_keep_display_label | hard | Rename type; keep telemetry string |
| multi_bug_cascade_undoc | hard | 3 bugs; docs mention only one; tight max_turns |
| adversarial_plan_traps | hard | Stacked README/QUICK traps vs REAL_SPEC |
| interval_schedule_suite | hard | Compatible / activity selection / merge |

## How to run

```bash
export GROKPTAH_LIVE_EVAL=1
export GROKPTAH_MODEL=grok-4.5
./evals/scripts/run_parity.sh
```

Offline (CI): `cargo test eval_oracle` in the bridge workspace + suite-shape check in Desktop workflow.

## CI gate policy

| Gate | Blocks merge? |
|------|----------------|
| Bridge fmt / clippy `-D warnings` / tests | **Yes** |
| Desktop tsc + vitest | **Yes** |
| Offline oracle unit tests + hard-task shape | **Yes** |
| Full live discriminating suite | **No** (on-demand; required for ≥ claims) |

## Latest live results — turn-efficiency cycle (#187/#188)

Model: **grok-4.5** · Suite: **14 tasks** · max_turns **unchanged** (multi_bug=3, cross_cut=4)  
Merge SHA (efficiency): `e34b0b0c73543d7ae039501e10344d51e80b93d4`  
Evidence: goal scratch `proof-1/`, `proof-2/`  
Instrumentation: `tool_names`, `cargo_test_ran`, `cargo_test_first_round` on both sides.

### Agent capability changes (this cycle)

- `write_files` multi-file batch tool (real dispatch path + unit tests)
- Efficiency system guidance + tool schema reorder (edit/test tools first)
- `HostConfig.max_agent_rounds` aligned with live_eval task budget + final-step tool filter (edit/shell only) + cargo-test-failure coaching

### proof-1

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success | **14/14** | **14/14** |

| Gap task | Ptah | CLI | Ptah ≥ CLI |
|----------|:----:|:---:|:----------:|
| multi_bug_cascade_undoc | ✓ | ✓ | **YES** |
| cross_cut_legacy_widget | ✓ | ✓ | **YES** |

### proof-2 (consistency)

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success | **13/14** | **14/14** |

| Gap task | Ptah | CLI | Ptah ≥ CLI |
|----------|:----:|:---:|:----------:|
| multi_bug_cascade_undoc | ✗ | ✓ | **NO** |
| cross_cut_legacy_widget | ✓ | ✓ | **YES** |

### Gap resolution

| Task | Issue | Outcome |
|------|-------|---------|
| cross_cut_legacy_widget | #188 | **Closed** — Ptah ≥ CLI on **both** proof runs (stable) |
| multi_bug_cascade_undoc | #187 | **Remains open** — still flaky under max_turns=3 (pass proof-1, fail proof-2); residual model noise despite write_files + budget coaching |

**Do not claim full suite ≥ Build.** #187 residual is honest.

### Prior history

Earlier discriminating baseline (pre-efficiency): Ptah often behind on both gap tasks. See git history / issue threads.

## Honesty / deliberate non-parity

| Area | Status |
|------|--------|
| OS sandbox | **Non-goal** |
| Full xai-grok-tools matrix | TOOL_MATRIX residual |
| #144 xai-chat-state lift | **NOT_PLANNED** (reopened & closed as not planned) |
| Uniform 100% as “parity” | **Rejected** — this suite must discriminate |

## Related

- `docs/TOOL_MATRIX.md`  
- `docs/ADR-001-agent-runtime.md`  
- `crates/codegen/grokptah-agent-bridge/src/eval_oracle.rs`  
