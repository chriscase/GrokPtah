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

## Latest live results — agent-quality cycle (#223 / #187 / #209)

Model: **grok-4.5** · Focused gap suite: `rename_keep_display_label` + `multi_bug_cascade_undoc` · max_turns **unchanged** (both = 3)  
Branch: `feat/agent-quality-parity-223-187-209`  
Evidence: scratch `proof-1/` (pre-reverify coaching), `proof-2/` + `proof-3/` (post-reverify gate)

### Agent capability changes (this cycle)

- Efficiency guidance: multi-bug batch-all-failures; rename preserves `PRODUCT_LABEL` / string literals; ban blind whole-tree sed
- Cargo-failure coaching lists distinct failing test names and requires batch fix + re-run
- **Re-verify gate:** after a cargo failure under tight budgets, do not accept Final until cargo is green again; post-edit coaching forces re-run
- Recovery grace allows edit **+ shell** so the re-run can happen in the bounded extra step
- **Evidence-backed terminal handoff:** `enrich_terminal_handoff` appends observed changed paths + cargo result when model/stop text omits them (including recovery-stop finals); live_eval quality findings clear
- #209 stationarity detector already on main (`IdenticalToolCallRun`, true-noop stop at 4, nudge at 8) — unit tests green

### Focused gap proofs (Ptah bridge)

| Run | rename o/v | multi_bug o/v | handoff quality |
|-----|:----------:|:-------------:|:---------------:|
| proof-6 (handoff fix) | ✓/✓ | ✓/✓ | **[]** |
| proof-7 (handoff fix) | ✓/✓ | ✓/✓ | **[]** |

### Dual-side focused baseline (proof-7 Ptah vs Grok CLI)

Model: **grok-4.5** · max_turns=3 · same fixtures/prompts

| Task | Ptah oracle | Ptah verified | CLI oracle | Ptah ≥ CLI |
|------|:-----------:|:-------------:|:----------:|:----------:|
| rename_keep_display_label | ✓ | ✓ | ✓ | **YES** |
| multi_bug_cascade_undoc | ✓ | ✓ | ✓ | **YES** |

### Full dual-side baselines (14 tasks, PR head `fe69fbb`)

Model: **grok-4.5** · identical fixtures/prompts/max_turns · YOLO both sides

| Run | Ptah oracle | Ptah verified | CLI oracle | Ptah ≥ CLI (success then tool_errors) |
|-----|------------:|--------------:|-----------:|:--------------------------------------|
| baseline-1 | **14/14** | 13/14 | **14/14** | NO (tool_errors 7 vs 0) |
| baseline-2 | **13/14** | 11/14 | **14/14** | NO (oracle + tool_errors) |

#### Per-task variance (oracle)

| Task | R1 Ptah | R1 CLI | R2 Ptah | R2 CLI | notes |
|------|:-------:|:------:|:-------:|:------:|-------|
| multi_bug_cascade_undoc | ✓ | ✓ | ✗ | ✓ | **Ptah flaky** under max_turns=3 |
| rename_keep_display_label | ✓ | ✓ | ✓ | ✓ | stable oracle; verified flaked R2 |
| long_horizon_trap_bug99 | ✓ | ✓ | ✓ | ✓ | oracle stable; verified false both runs |
| all other tasks (11) | ✓ | ✓ | ✓ | ✓ | stable |

#### Residual gaps (honest)

1. **#187 multi_bug** residual after harden:
   - Mid-batch skip explore **and shell** until an edit lands after cargo fail.
   - **Host auto cargo re-verify** after successful edit while cargo was red.
   - Focused live (post-auto-reverify): verified can go true (e.g. runs 11, 16, 18, 22) when multi-file fixes are complete; oracle still flaky when the model under-fixes with partial `write_file` under max_turns=3 (not thrash).
2. **Verified signal** residual: long_horizon on full baselines; multi_bug when fixes incomplete (tests stay red).
3. **tool_errors** higher on Ptah than CLI when success ties.
4. **#209** unit-verified on main; no live intentional-noop injection.
5. **#223 rename** full dual-side oracle stable both baselines; focused dual-side ≥ CLI.
6. **Do not claim full suite ≥ Grok Build** — multi_bug still model-variance under max_turns=3.

Evidence: goal scratch `baselines/run{1,2}/`, `multibug-harden/run-*`.

### Prior cycle — turn-efficiency (#187/#188)

Merge SHA (efficiency): `e34b0b0c73543d7ae039501e10344d51e80b93d4`

| Task | Issue | Prior outcome |
|------|-------|---------------|
| cross_cut_legacy_widget | #188 | **Closed** — stable ≥ CLI |
| multi_bug_cascade_undoc | #187 | Was flaky under max_turns=3 |

**Do not claim full 14-task suite ≥ Build without a fresh dual-side full run.** This cycle proves the two open gap tasks on Ptah under existing budgets.

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
