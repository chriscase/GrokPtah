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
# optional: GROKPTAH_MODEL=grok-build  GROK_BIN=$(which grok)
./evals/scripts/run_parity.sh
```

Outputs under `evals/runs/<timestamp>/`:

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

_Populate after each harness run. Do not invent numbers._

| Date (UTC) | SHA | Model | Ptah success | CLI success | Ptah ≥ CLI? | Run dir |
|------------|-----|-------|-------------:|------------:|:-----------:|---------|
| _pending first live run_ | | | | | | |

## Honesty / deliberate non-parity

| Area | Status |
|------|--------|
| OS sandbox / Landlock | **Non-goal** — exec-risk + soft tool-safety only (#155) |
| Full `xai-grok-tools` matrix | See `TOOL_MATRIX.md` residual policy (#160) |
| Workflows engine | Deferred (#176) |
| Personas depth vs Build | Partial while #164 in progress |

## Related docs

- `docs/PARITY_EVALS.md` — offline smokes  
- `docs/TOOL_MATRIX.md` — tool residual policy  
- `docs/ADR-001-agent-runtime.md` — thin loop strategy  
