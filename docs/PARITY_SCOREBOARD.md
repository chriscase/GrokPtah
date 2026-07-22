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

## Latest live results (discriminating)

Model: **grok-4.5** · Suite: **14 tasks** (2 smoke + 12 hard)  
Evidence: goal scratch `eval-final-A/`, `eval-final-B/`  
max_turns enforced on bridge (`round > max_turns` cancel) and CLI `--max-turns`.

### Run A — eval-final-A

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success | **13/14** | **14/14** |
| Hard-only | 11/12 | 12/12 |
| Tool errors | 0 | 0 |

**Ptah ≥ CLI:** **NO**

| Task | Ptah | CLI |
|------|:----:|:---:|
| multi_bug_cascade_undoc | ✗ | ✓ |
| (all others) | ✓ | ✓ |

### Run B — eval-final-B (consistency)

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success | **12/14** | **14/14** |
| Hard-only | 10/12 | 12/12 |

**Ptah ≥ CLI:** **NO**

| Task | Ptah | CLI |
|------|:----:|:---:|
| cross_cut_legacy_widget | ✗ | ✓ |
| multi_bug_cascade_undoc | ✗ | ✓ |
| (all others) | ✓ | ✓ |

### Consistency note

- **multi_bug_cascade_undoc** failed Ptah on **both** runs (stable gap).  
- **cross_cut_legacy_widget** failed Ptah on run B only (model noise under tight budget). Documented; not averaged away.  
- Suite is **non-uniform** (not 100% both sides). Claim **≥ Grok Build: NO** on this suite.

### Earlier runs (pre-fix max_turns off-by-one)

`eval-run-3` / `eval-run-4` used `round >= max_turns` (one fewer model step than CLI). Fixed to `round > max_turns` before final A/B. Do not use those aggregates for claims.

## Gaps (Ptah behind CLI)

| Task | Status | Action |
|------|--------|--------|
| multi_bug_cascade_undoc | Stable fail under max_turns=3 | [#187](https://github.com/chriscase/GrokPtah/issues/187) |
| cross_cut_legacy_widget | Flaky under max_turns=4 | [#188](https://github.com/chriscase/GrokPtah/issues/188) |

Agent-side mitigations already shipped: system-prompt batching guidance; max_turns parity fix in live_eval.

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
