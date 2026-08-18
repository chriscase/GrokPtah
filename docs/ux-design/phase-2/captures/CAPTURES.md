# Prototype capture index

Deterministic renders of the Phase 2 prototype, produced by
[capture.sh](capture.sh) with headless Google Chrome
(`--headless=new --force-prefers-reduced-motion --virtual-time-budget=3000`).
Fixture data uses fixed timestamps and reduced motion is forced, so repeated
runs are visually identical. Desktop frames are 1440×900; narrow frames are
760×1000.

Regenerate at any time:

```bash
docs/ux-design/phase-2/captures/capture.sh
```

| Capture | Route | What it evidences |
|---|---|---|
| `d1-focused-lane-running.png` | `#/d1/lane/lane-1` | Direction 1 default: context header (Lane · Agent · Runtime · Workspace · Run), transcript with tool/test turns, explicit composer target, drawer dock with badges |
| `d1-approvals-drawer.png` | `#/d1/lane/lane-2?drawer=approvals` | Awaiting-approval banner + Approvals drawer bound to Run r-402 and file fingerprint; Review-diff-first default |
| `d1-missing-workspace.png` | `#/d1/lane/lane-5` | Missing-workspace recovery: blocked chip, banner with Choose folder, technical path behind details, composer disabled with stated reason |
| `d1-interrupted-checkpoint.png` | `#/d1/lane/lane-4` | Interrupted Run with verified checkpoint c-77; Resume vs Retry distinguished in copy |
| `d1-queued-run.png` | `#/d1/lane/lane-9` | Queued Run at position 2 on the hosted service, cancel offered |
| `d1-stale-stream.png` | `#/d1/lane/lane-2?conn=stale` | Reconnecting event stream: "from last durable cursor" copy, refresh action |
| `d1-expert-grid.png` | `#/d1/grid` | Opt-in expert Grid: two zones, each with own header and composer; scope bar |
| `d2-agent-roster.png` | `#/d2/agents` | Direction 2 home: summary strip, agent cards with lifecycle ≠ health chips, runtime + connection, lane counts, checkpoint or "none yet", ad-hoc section |
| `d2-agents-empty.png` | `#/d2/agents?demo=empty` | True empty state ("No durable Agents yet") with constructive actions |
| `d2-agents-load-failed.png` | `#/d2/agents?demo=error` | Load-failed state that **replaces** the roster/empty state; Retry + Technical details containing the demoted store-lock diagnostics |
| `d2-agent-detail-attention.png` | `#/d2/agent/agent-2` | Agent detail: identity/policy/memory/checkpoint, Start-Lane form with runtime targets and live connection chips, Lanes grouped by Attention/Active |
| `d2-agent-retired.png` | `#/d2/agent/agent-4` | Retired Agent: banner, blocked new work with reason, history preserved |
| `d2-lane-archived.png` | `#/d2/lane/lane-6` | Archived Lane: preservation copy, Restore action, composer blocked with restore guidance |
| `d2-lanes-archived-view.png` | `#/d2/lanes/archived` | Archive view: preservation banner, archived rows (incl. "Scratch workspace" label hygiene), Restore actions |
| `d2-lane-disconnected-vm.png` | `#/d2/lane/lane-10` | Disconnected local service/VM: last-seen, reconnect or switch-runtime, durable-history copy |
| `d3-supervision-workspace.png` | `#/d3/workspace` | Direction 3 default: scope bar, two self-labeled zones with composers, Inspector pinned to a named Lane |
| `d3-runtime-targets.png` | `#/d3/runtime` | Runtime targets: connection, workspace authority, "what syncs" boundaries, support matrix, per-Lane runtime table |
| `narrow-d1-focused-lane.png` | `#/d1/lane/lane-1` @760px | Narrow-window behavior: horizontal rail strip, icon-only dock (names preserved for AT), stacked composer |
| `narrow-d2-agent-roster.png` | `#/d2/agents` @760px | Narrow roster: single-column cards, collapsed direction switcher |

Evidence note: these are renders of fixture data in a static prototype.
They demonstrate design intent only — in particular, hosted-service and
service/VM frames illustrate the documented service contract, and Computer
Use appears only in its audited unavailable state.
