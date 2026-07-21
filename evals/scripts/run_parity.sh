#!/usr/bin/env bash
# Live head-to-head: GrokPtah bridge vs grok CLI (#171/#172/#173).
#
# Usage:
#   GROKPTAH_LIVE_EVAL=1 ./evals/scripts/run_parity.sh [run_id]
#
# Env:
#   GROK_BIN       default: grok on PATH, else cargo run -p xai-grok-pager-bin
#   GROKPTAH_MODEL default: grok-build
#   SCRATCH        optional output dir (default: evals/runs/<timestamp>)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RUN_ID="${1:-$(date -u +%Y%m%dT%H%M%SZ)}"
OUT="${SCRATCH:-$ROOT/evals/runs/$RUN_ID}"
mkdir -p "$OUT"

# Prefer an id the installed CLI accepts (`grok models`). Override with GROKPTAH_MODEL.
MODEL="${GROKPTAH_MODEL:-grok-4.5}"
TASKS="$ROOT/evals/tasks.json"
FIXTURES="$ROOT/evals/fixtures"
BRIDGE="$ROOT/crates/codegen/grokptah-agent-bridge"

if [[ -z "${GROKPTAH_LIVE_EVAL:-}" ]]; then
  echo "error: set GROKPTAH_LIVE_EVAL=1" >&2
  exit 2
fi

echo "== Ptah bridge live eval =="
(
  cd "$BRIDGE"
  GROKPTAH_LIVE_EVAL=1 cargo run --example live_eval --quiet -- \
    --tasks "$TASKS" \
    --fixtures "$FIXTURES" \
    --out "$OUT/ptah.json" \
    --model "$MODEL"
) | tee "$OUT/eval-ptah.log"

echo "== grok CLI live eval =="
GROK_BIN="${GROK_BIN:-}"
if [[ -z "$GROK_BIN" ]]; then
  if command -v grok >/dev/null 2>&1; then
    GROK_BIN="$(command -v grok)"
  else
    # Build/run from tree (slow first time).
    GROK_BIN="cargo"
  fi
fi

python3 - "$TASKS" "$FIXTURES" "$OUT" "$GROK_BIN" "$MODEL" "$ROOT" <<'PY'
import json, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path

tasks_path, fixtures, out, grok_bin, model, root = sys.argv[1:7]
tasks = json.loads(Path(tasks_path).read_text())
results = []

def check_success(work: Path, spec: dict) -> bool:
    if spec.get("type") != "file_contains":
        return False
    p = work / spec["path"]
    if not p.is_file():
        return False
    body = p.read_text(errors="replace")
    for s in spec.get("must_contain", []):
        if s not in body:
            return False
    for s in spec.get("must_not_contain", []):
        if s in body:
            return False
    for s in spec.get("must_not_remove", []):
        if s not in body:
            return False
    return True

for task in tasks:
    fixture = Path(fixtures) / task["fixture"]
    work = Path(tempfile.mkdtemp(prefix=f"cli-{task['id']}-"))
    shutil.copytree(fixture, work, dirs_exist_ok=True)
    prompt = task["prompt"]
    max_turns = str(task.get("max_turns", 12))
    t0 = time.time()
    err = None
    if grok_bin == "cargo":
        cmd = [
            "cargo", "run", "-q", "-p", "xai-grok-pager-bin", "--",
            "-p", prompt,
            "--cwd", str(work),
            "-m", model,
            "--yolo",
            "--max-turns", max_turns,
            "--output-format", "plain",
            "--no-auto-update",
        ]
        cwd = root
    else:
        cmd = [
            grok_bin, "-p", prompt,
            "--cwd", str(work),
            "-m", model,
            "--yolo",
            "--max-turns", max_turns,
            "--output-format", "plain",
            "--no-auto-update",
        ]
        cwd = None
    try:
        proc = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=300
        )
        if proc.returncode != 0:
            err = f"exit {proc.returncode}: {proc.stderr[-500:]}"
        log = (proc.stdout or "") + "\n" + (proc.stderr or "")
    except Exception as e:
        err = str(e)
        log = ""
    wall_ms = int((time.time() - t0) * 1000)
    success = check_success(work, task["success"])
    # Heuristic tool metrics from log (CLI does not always emit structured counts)
    tool_calls = log.count("run_terminal_cmd") + log.count("search_replace") + log.count("read_file") + log.count("Write") + log.count("write_file")
    tool_errors = log.lower().count("error") // 5  # coarse
    results.append({
        "id": task["id"],
        "success": success,
        "wall_ms": wall_ms,
        "tool_calls": tool_calls,
        "tool_errors": tool_errors,
        "permission_prompts": 0,  # yolo
        "rounds_est": 0,
        "error": err,
        "detail": "ok" if success else "success predicate failed",
    })
    Path(out, f"cli-{task['id']}.log").write_text(log)
    shutil.rmtree(work, ignore_errors=True)

report = {
    "side": "grok-cli",
    "model": model,
    "grok_bin": grok_bin,
    "results": results,
}
Path(out, "cli.json").write_text(json.dumps(report, indent=2))
print(json.dumps(report, indent=2))
PY
tee "$OUT/eval-cli.log" < "$OUT/cli.json" >/dev/null || true
cp -f "$OUT/cli.json" "$OUT/eval-cli.log" 2>/dev/null || true

echo "== scoreboard =="
python3 - "$OUT" <<'PY'
import json, sys
from pathlib import Path
from datetime import datetime, timezone
out = Path(sys.argv[1])
ptah = json.loads((out / "ptah.json").read_text())
cli = json.loads((out / "cli.json").read_text())

def summarize(rep):
    rs = rep["results"]
    n = len(rs)
    ok = sum(1 for r in rs if r["success"])
    tools_e = sum(r.get("tool_errors") or 0 for r in rs)
    wall = sum(r.get("wall_ms") or 0 for r in rs)
    perms = sum(r.get("permission_prompts") or 0 for r in rs)
    rounds = sum(r.get("rounds_est") or 0 for r in rs)
    return {
        "tasks": n,
        "success": ok,
        "success_rate": (ok / n) if n else 0.0,
        "tool_errors": tools_e,
        "wall_ms_total": wall,
        "permission_prompts": perms,
        "rounds_est_total": rounds,
        "per_task": rs,
    }

ps, cs = summarize(ptah), summarize(cli)
# ≥ Build: primary = success_rate, then fewer tool_errors, then wall
ge = (
    ps["success_rate"] > cs["success_rate"]
    or (
        ps["success_rate"] == cs["success_rate"]
        and ps["tool_errors"] <= cs["tool_errors"]
    )
)

md = []
md.append(f"# Parity scoreboard (live)")
md.append("")
md.append(f"- Generated: `{datetime.now(timezone.utc).isoformat()}`")
md.append(f"- Run dir: `{out}`")
md.append(f"- Model: `{ptah.get('model')}`")
md.append(f"- Ptah side: `{ptah.get('side')}`")
md.append(f"- CLI side: `{cli.get('side')}` bin=`{cli.get('grok_bin')}`")
md.append("")
md.append("## Aggregate")
md.append("")
md.append("| Metric | GrokPtah | Grok CLI |")
md.append("|--------|---------:|---------:|")
md.append(f"| Success count | {ps['success']}/{ps['tasks']} | {cs['success']}/{cs['tasks']} |")
md.append(f"| Success rate | {ps['success_rate']:.0%} | {cs['success_rate']:.0%} |")
md.append(f"| Tool errors (sum) | {ps['tool_errors']} | {cs['tool_errors']} |")
md.append(f"| Wall ms (sum) | {ps['wall_ms_total']} | {cs['wall_ms_total']} |")
md.append(f"| Permission prompts | {ps['permission_prompts']} | {cs['permission_prompts']} |")
md.append(f"| Rounds est (sum) | {ps['rounds_est_total']} | {cs['rounds_est_total']} |")
md.append("")
md.append(f"**Ptah ≥ CLI (success, then tool_errors):** `{'YES' if ge else 'NO'}`")
md.append("")
md.append("## Per task")
md.append("")
md.append("| Task | Ptah | CLI | Ptah wall ms | CLI wall ms |")
md.append("|------|:----:|:---:|-------------:|------------:|")
by_p = {r["id"]: r for r in ps["per_task"]}
by_c = {r["id"]: r for r in cs["per_task"]}
for tid in sorted(set(by_p) | set(by_c)):
    p, c = by_p.get(tid, {}), by_c.get(tid, {})
    md.append(
        f"| {tid} | {'✓' if p.get('success') else '✗'} | {'✓' if c.get('success') else '✗'} | {p.get('wall_ms','')} | {c.get('wall_ms','')} |"
    )
md.append("")
md.append("## Honesty notes")
md.append("")
md.append("- Live network + credentials required; offline smokes are not parity.")
md.append("- CLI tool_errors are log heuristics when structured metrics unavailable.")
md.append("- Exec-risk / soft tool-safety is **not** OS sandbox parity.")
md.append("")

text = "\n".join(md) + "\n"
(out / "scoreboard.md").write_text(text)
# Also write machine-readable
summary = {"ptah": ps, "cli": cs, "ptah_ge_cli": ge}
(out / "scoreboard.json").write_text(json.dumps(summary, indent=2))
print(text)
if not ge:
    sys.exit(1)
PY

echo "Wrote $OUT/scoreboard.md"
