# Parity scoreboard (live)

**Program:** Phase 16 epic [#153](https://github.com/chriscase/GrokPtah/issues/153)  
**Harness:** [#171](https://github.com/chriscase/GrokPtah/issues/171) · fixtures [#172](https://github.com/chriscase/GrokPtah/issues/172) · this doc [#173](https://github.com/chriscase/GrokPtah/issues/173)

## Definition of “≥ Grok Build”

On the **fixed fixture set** in `evals/tasks.json`, GrokPtah Build is ≥ Grok Build CLI when:

1. **Success rate** ≥ CLI success rate, and  
2. If success rates are equal, **tool_errors** (sum) ≤ CLI,  

with both sides run **live** (network + credentials), same model id, YOLO / always-approve so permission friction is comparable.

**Offline smokes are not parity.** They gate regressions only.

## How to run

```bash
export GROKPTAH_LIVE_EVAL=1
export GROKPTAH_MODEL=grok-4.5   # must be in `grok models` for the CLI side
./evals/scripts/run_parity.sh
```

Outputs under `evals/runs/<timestamp>/` or `$SCRATCH`:

| File | Meaning |
|------|---------|
| `ptah.json` | Bridge headless metrics |
| `cli.json` | `grok` CLI metrics |
| `scoreboard.md` / `scoreboard.json` | Aggregate comparison |

Two consecutive runs should agree on per-task pass/fail before claiming a ship gate.

## CI gate policy (#177 / #173)

| Gate | Blocks merge? |
|------|----------------|
| `cargo fmt --check` (bridge workspace) | **Yes** |
| `cargo clippy --all-targets -- -D warnings` | **Yes** |
| `cargo test` (bridge + desktop src-tauri) | **Yes** |
| desktop `tsc` + vitest | **Yes** |
| Live parity eval | **No** (manual / `GROKPTAH_LIVE_EVAL=1`; required to close #153) |

Red CI is blocking. Do not merge while Desktop workflow is red.

## Latest live results

Model: **grok-4.5** (installed CLI did not accept `grok-build` id — see honesty notes).  
SHA at run: `8191e28` (phase-16/parity).

### Run A — 2026-07-21T23:30:10Z

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success count | 3/3 | 3/3 |
| Success rate | 100% | 100% |
| Tool errors (sum) | 0 | 0 |
| Wall ms (sum) | 19829 | 47802 |
| Permission prompts | 0 | 0 |

**Ptah ≥ CLI:** YES (equal success; equal tool_errors; Ptah faster wall).

| Task | Ptah | CLI | Ptah ms | CLI ms |
|------|:----:|:---:|--------:|-------:|
| basic_edit_add_mul | ✓ | ✓ | 7055 | 9289 |
| search_edit_version | ✓ | ✓ | 7626 | 30482 |
| basic_edit_readme_token | ✓ | ✓ | 5148 | 8031 |

### Run B — 2026-07-21T23:30:53Z (consistency)

| Metric | GrokPtah | Grok CLI |
|--------|---------:|---------:|
| Success count | 3/3 | 3/3 |
| Success rate | 100% | 100% |
| Tool errors (sum) | 0 | 0 |
| Wall ms (sum) | 16562 | 25293 |

**Ptah ≥ CLI:** YES. Per-task pass/fail **identical** to Run A.

Raw logs: goal scratch `eval-run-2/`, `eval-run-3/`.

## Honesty / deliberate non-parity

| Area | Status |
|------|--------|
| OS sandbox / Landlock | **Non-goal** — exec-risk + soft tool-safety only (#155) |
| Full `xai-grok-tools` matrix | See `TOOL_MATRIX.md` residual policy (#160) |
| Workflows engine | Deferred (#176) |
| Model id `grok-build` on installed CLI | CLI 0.2.103 listed only `grok-4.5`; evals used `grok-4.5` for a fair head-to-head |
| Fixture set size | Three coding tasks — not the entire Build surface |

## Related docs

- `docs/PARITY_EVALS.md` — offline smokes  
- `docs/TOOL_MATRIX.md` — tool residual policy  
- `docs/ADR-001-agent-runtime.md` — thin loop strategy  
