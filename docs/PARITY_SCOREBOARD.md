# Parity scoreboard (live)

**Program:** Continuous improvement — real ≥ Grok Build proof  
**Harness:** `evals/scripts/run_parity.sh` + bridge `examples/live_eval.rs`  
**Related:** [#185](https://github.com/chriscase/GrokPtah/issues/185)

## Definition of “≥ Grok Build”

On the **fixture set** in `evals/tasks.json`, GrokPtah Build is ≥ Grok Build CLI when:

1. **Success rate** ≥ CLI success rate, and  
2. If success rates are equal, **tool_errors** (sum) ≤ CLI,  

with both sides run **live** (network + credentials), same model id, YOLO / always-approve.

**Offline smokes are not parity.** Three-task smoke alone is not a ≥ claim.

## Suite composition (non-trivial)

| Task id | Type |
|---------|------|
| basic_edit_* / search_edit_* | Smoke edits (kept) |
| multi_file_rename_app | Multi-file refactor |
| plan_then_execute_util | Plan → execute |
| test_and_fix_max | Test-and-fix |
| fail_recover_notes | Failure/recovery style (read + alternate artifact) |
| long_horizon_bug42 | Long multi-tool (read docs → fix → report) |

## How to run

```bash
export GROKPTAH_LIVE_EVAL=1
export GROKPTAH_MODEL=grok-4.5   # must appear in `grok models`
./evals/scripts/run_parity.sh
```

## CI gate policy

| Gate | Blocks merge? |
|------|----------------|
| `cargo fmt --check` (bridge) | **Yes** |
| `cargo clippy --all-targets -- -D warnings` | **Yes** |
| `cargo test` (bridge + desktop src-tauri) | **Yes** |
| desktop `tsc` + vitest | **Yes** |
| Live parity eval | **No** (required for ≥ claim / continuous cycles) |

## Latest live results (non-trivial suite)

Model: **grok-4.5** · Suite size: **8 tasks** · SHA at doc write: see merge of continuous/parity-real

### Run A — 2026-07-22T08:48:00Z

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success count | 8/8 | 8/8 |
| Success rate | 100% | 100% |
| Tool errors (sum) | 0 | 0 |
| Wall ms (sum) | 71059 | 95479 |
| Permission prompts | 0 | 0 |

**Ptah ≥ CLI:** **YES** (equal success + tool_errors; Ptah lower wall).

### Run B — 2026-07-22T08:51:06Z (consistency)

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success count | 8/8 | 8/8 |
| Success rate | 100% | 100% |
| Tool errors (sum) | 0 | 0 |
| Wall ms (sum) | 69352 | 108748 |

**Ptah ≥ CLI:** **YES**. Per-task pass/fail **identical** to Run A (all ✓).

Raw: goal scratch `eval-run-1b/`, `eval-run-2/`.

### Prior note (Run pre-fix, long_horizon only)

An earlier full run failed long_horizon on Ptah due to a **false-negative predicate** (`must_not_contain: "out>"` matched a comment). Predicate fixed to `format!("out>`; suite re-run for honest 8/8.

## Honesty / deliberate non-parity

| Area | Status |
|------|--------|
| OS sandbox / Landlock | **Non-goal** — exec-risk + soft tool-safety only |
| Full xai-grok-tools matrix | See TOOL_MATRIX residual policy |
| Workflows engine | Deferred |
| #144 full xai-chat-state lift | **NOT PLANNED** unless hand-port cost forces it (ADR-001) |
| #145 host.rs ~5.2k | **Accept-as-is residual** after `host_helpers` extract (ADR-001) |
| Suite scope | 8 tasks — not entire Build product surface |

## Related docs

- `docs/PARITY_EVALS.md` — offline smokes  
- `docs/TOOL_MATRIX.md`  
- `docs/ADR-001-agent-runtime.md`  
