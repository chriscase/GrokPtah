#!/usr/bin/env bash
# Live head-to-head: GrokPtah bridge vs grok CLI (discriminating suite).
#
# Usage:
#   GROKPTAH_LIVE_EVAL=1 ./evals/scripts/run_parity.sh [run_id]
#
# Env:
#   GROK_BIN       default: grok on PATH
#   GROKPTAH_MODEL default: grok-4.5
#   SCRATCH        optional output dir (default: evals/runs/<timestamp>)
#   Does NOT exit non-zero when Ptah < CLI — honest reporting only.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RUN_ID="${1:-$(date -u +%Y%m%dT%H%M%SZ)}"
OUT="${SCRATCH:-$ROOT/evals/runs/$RUN_ID}"
mkdir -p "$OUT"

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
    GROK_BIN="cargo"
  fi
fi

python3 - "$TASKS" "$FIXTURES" "$OUT" "$GROK_BIN" "$MODEL" "$ROOT" "$BRIDGE" <<'PY'
import json, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path

tasks_path, fixtures, out, grok_bin, model, root, bridge = sys.argv[1:8]
tasks = json.loads(Path(tasks_path).read_text())
results = []

def run_oracle(work: Path, success: dict) -> tuple[bool, str]:
    """Structured oracles mirrored from eval_oracle.rs (single source of intent)."""
    ty = success.get("type")
    if ty == "file_contains":
        return check_file_contains(work, success)
    if ty == "exact_file":
        return check_exact_file(work, success)
    if ty == "command":
        return check_command(work, success)
    if ty == "file_exists":
        p = work / success["path"]
        ok = p.is_file()
        return ok, ("exists " if ok else "missing ") + success["path"]
    if ty == "file_absent":
        p = work / success["path"]
        ok = not p.exists()
        return ok, ("absent " if ok else "unexpected ") + success["path"]
    if ty == "all":
        parts = []
        for i, c in enumerate(success.get("checks") or []):
            ok, detail = run_oracle(work, c)
            parts.append(f"[{i}] {detail}")
            if not ok:
                return False, "; ".join(parts)
        return True, "; ".join(parts)
    return False, f"unknown oracle type {ty!r}"

def check_file(work, path, must_contain=None, must_not_contain=None, must_not_remove=None):
    p = work / path
    if not p.is_file():
        return False, f"cannot read {path}"
    body = p.read_text(errors="replace")
    for s in must_contain or []:
        if s not in body:
            return False, f"missing must_contain in {path}: {s!r}"
    for s in must_not_contain or []:
        if s in body:
            return False, f"hit must_not_contain in {path}: {s!r}"
    for s in must_not_remove or []:
        if s not in body:
            return False, f"missing must_not_remove in {path}: {s!r}"
    return True, "ok"

def check_file_contains(work, spec):
    ok, d = check_file(
        work,
        spec["path"],
        spec.get("must_contain"),
        spec.get("must_not_contain"),
        spec.get("must_not_remove"),
    )
    if not ok:
        return False, d
    for extra in spec.get("extra_checks") or []:
        ok, d = check_file(
            work,
            extra["path"],
            extra.get("must_contain"),
            extra.get("must_not_contain"),
            extra.get("must_not_remove"),
        )
        if not ok:
            return False, f"extra {extra['path']}: {d}"
    return True, "file_contains ok"

def check_exact_file(work, spec):
    path = spec["path"]
    p = work / path
    if not p.is_file():
        return False, f"exact_file: cannot read {path}"
    actual = p.read_text(errors="replace").replace("\r\n", "\n").replace("\r", "\n")
    if "expected" in spec and spec["expected"] is not None:
        expected = str(spec["expected"]).replace("\r\n", "\n").replace("\r", "\n")
    elif spec.get("expected_from"):
        ep = work / spec["expected_from"]
        if not ep.is_file():
            return False, f"exact_file: cannot read expected_from {spec['expected_from']}"
        expected = ep.read_text(errors="replace").replace("\r\n", "\n").replace("\r", "\n")
    else:
        return False, "exact_file: need expected or expected_from"
    if actual == expected:
        return True, f"exact_file {path}"
    return False, f"exact_file mismatch {path} (actual {len(actual)} bytes, expected {len(expected)} bytes)"

def check_command(work, spec):
    argv = spec.get("argv") or []
    if not argv:
        return False, "command: empty argv"
    want = int(spec.get("exit_code", 0))
    timeout = int(spec.get("timeout_secs", 120))
    env = os.environ.copy()
    env["CARGO_TERM_COLOR"] = "never"
    try:
        proc = subprocess.run(
            argv,
            cwd=str(work),
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired:
        return False, f"command timeout after {timeout}s: {argv}"
    except Exception as e:
        return False, f"command error: {e}"
    code = proc.returncode if proc.returncode is not None else -1
    tail = (proc.stderr or "")[-400:]
    if code == want:
        return True, f"command ok: {argv}; exit={code}"
    return False, f"command fail: {argv}; exit={code} want={want}; stderr_tail={tail}"

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
            cmd, cwd=cwd, capture_output=True, text=True, timeout=420
        )
        if proc.returncode != 0:
            err = f"exit {proc.returncode}: {(proc.stderr or '')[-500:]}"
        log = (proc.stdout or "") + "\n" + (proc.stderr or "")
    except Exception as e:
        err = str(e)
        log = ""
    wall_ms = int((time.time() - t0) * 1000)
    success, detail = run_oracle(work, task["success"])
    # Heuristic tool inventory from CLI log (#187/#188 instrumentation).
    tool_names = []
    for name in (
        "run_terminal_cmd",
        "search_replace",
        "read_file",
        "write_file",
        "write_files",
        "apply_patch",
        "grep",
        "list_dir",
        "glob_files",
    ):
        n = log.count(name)
        tool_names.extend([name] * n)
    tool_calls = len(tool_names) if tool_names else (
        log.count("Write") // 2
    )
    tool_errors = log.lower().count("error") // 5
    cargo_test_ran = "cargo test" in log.lower() or "cargo\ttest" in log.lower()
    # CLI does not always emit structured rounds; leave null when unknown.
    cargo_test_first_round = 1 if cargo_test_ran else None
    detail = (
        f"{detail}; tools=[{','.join(tool_names[:40])}]; "
        f"cargo_test={cargo_test_ran}; cargo_test_round={cargo_test_first_round!r}"
    )
    results.append({
        "id": task["id"],
        "success": success,
        "wall_ms": wall_ms,
        "tool_calls": tool_calls,
        "tool_errors": tool_errors,
        "permission_prompts": 0,
        "rounds_est": 0,
        "error": err,
        "detail": detail,
        "difficulty": task.get("difficulty"),
        "tool_names": tool_names,
        "cargo_test_ran": cargo_test_ran,
        "cargo_test_first_round": cargo_test_first_round,
    })
    Path(out, f"cli-{task['id']}.log").write_text(log)
    # Keep failed trees when debugging
    if not success and os.environ.get("GROKPTAH_EVAL_KEEP_FAIL"):
        dump = Path(out) / f"cli-fail-{task['id']}"
        if dump.exists():
            shutil.rmtree(dump, ignore_errors=True)
        shutil.copytree(work, dump)
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
cp -f "$OUT/cli.json" "$OUT/eval-cli.log" 2>/dev/null || true

echo "== scoreboard =="
python3 - "$OUT" "$TASKS" <<'PY'
import json, sys
from pathlib import Path
from datetime import datetime, timezone
out = Path(sys.argv[1])
tasks_list = json.loads(Path(sys.argv[2]).read_text())
tasks_meta = {t["id"]: t for t in tasks_list}
task_order = [t["id"] for t in tasks_list]
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
    hard = [r for r in rs if (r.get("difficulty") or tasks_meta.get(r["id"], {}).get("difficulty")) != "smoke"]
    hard_ok = sum(1 for r in hard if r["success"])
    return {
        "tasks": n,
        "success": ok,
        "success_rate": (ok / n) if n else 0.0,
        "hard_tasks": len(hard),
        "hard_success": hard_ok,
        "tool_errors": tools_e,
        "wall_ms_total": wall,
        "permission_prompts": perms,
        "rounds_est_total": rounds,
        "per_task": rs,
    }

ps, cs = summarize(ptah), summarize(cli)
ge = (
    ps["success_rate"] > cs["success_rate"]
    or (
        ps["success_rate"] == cs["success_rate"]
        and ps["tool_errors"] <= cs["tool_errors"]
    )
)
uniform_both = ps["success"] == ps["tasks"] and cs["success"] == cs["tasks"] and ps["tasks"] > 0

md = []
md.append(f"# Parity scoreboard (live run artifact)")
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
md.append(f"| Hard-only success | {ps['hard_success']}/{ps['hard_tasks']} | {cs['hard_success']}/{cs['hard_tasks']} |")
md.append(f"| Tool errors (sum) | {ps['tool_errors']} | {cs['tool_errors']} |")
md.append(f"| Wall ms (sum) | {ps['wall_ms_total']} | {cs['wall_ms_total']} |")
md.append("")
md.append(f"**Ptah ≥ CLI (success, then tool_errors):** `{'YES' if ge else 'NO'}`")
md.append(f"**Uniform 100% both sides:** `{'YES — suite may be too easy' if uniform_both else 'NO (discriminating)'}`")
md.append("")
md.append("## Per task")
md.append("")
md.append("| Task | Diff | Ptah | CLI | Ptah wall | CLI wall | Rationale |")
md.append("|------|------|:----:|:---:|----------:|---------:|-----------|")
by_p = {r["id"]: r for r in ps["per_task"]}
by_c = {r["id"]: r for r in cs["per_task"]}
for tid in task_order:
    p, c = by_p.get(tid, {}), by_c.get(tid, {})
    meta = tasks_meta.get(tid, {})
    diff = meta.get("difficulty") or p.get("difficulty") or "?"
    rat = (meta.get("difficulty_rationale") or "")[:80]
    md.append(
        f"| {tid} | {diff} | {'✓' if p.get('success') else '✗'} | {'✓' if c.get('success') else '✗'} | {p.get('wall_ms','')} | {c.get('wall_ms','')} | {rat} |"
    )
md.append("")
md.append("## Honesty notes")
md.append("")
md.append("- Live network + credentials required; offline oracle unit tests are not parity.")
md.append("- Hard tasks use structured oracles (cargo test / exact_file / composite), not prose substrings.")
md.append("- Do not weaken tasks to manufacture greens.")
md.append("- Exec-risk / soft tool-safety is **not** OS sandbox parity.")
md.append("")

text = "\n".join(md) + "\n"
(out / "scoreboard.md").write_text(text)
summary = {
    "ptah": ps,
    "cli": cs,
    "ptah_ge_cli": ge,
    "uniform_both_100": uniform_both,
}
(out / "scoreboard.json").write_text(json.dumps(summary, indent=2))
print(text)
# Always exit 0 after successful harness run — behind CLI is a finding, not a script failure.
sys.exit(0)
PY

echo "Wrote $OUT/scoreboard.md"
