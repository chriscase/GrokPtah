#!/bin/sh
# Regenerate the Phase 2 prototype captures deterministically.
# Requires Google Chrome on macOS; no other dependencies.
# Fixture data contains only fixed timestamps, and reduced motion is
# forced, so repeated runs produce visually identical frames.
set -eu

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
HERE="$(cd "$(dirname "$0")" && pwd)"
PROTO="file://$HERE/../prototype/index.html"

shot() { # name route widthxheight
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars \
    --force-prefers-reduced-motion --virtual-time-budget=3000 \
    --window-size="$3" --screenshot="$HERE/$1.png" "$PROTO#$2" 2>/dev/null
  echo "captured $1.png"
}

# Direction 1 — Focused Lane Workbench
shot d1-focused-lane-running        "/d1/lane/lane-1"                    1440,900
shot d1-approvals-drawer            "/d1/lane/lane-2?drawer=approvals"   1440,900
shot d1-missing-workspace           "/d1/lane/lane-5"                    1440,900
shot d1-interrupted-checkpoint      "/d1/lane/lane-4"                    1440,900
shot d1-queued-run                  "/d1/lane/lane-9"                    1440,900
shot d1-stale-stream                "/d1/lane/lane-2?conn=stale"         1440,900
shot d1-expert-grid                 "/d1/grid"                           1440,900

# Direction 2 — Agent Operations Home
shot d2-agent-roster                "/d2/agents"                         1440,900
shot d2-agents-empty                "/d2/agents?demo=empty"              1440,900
shot d2-agents-load-failed          "/d2/agents?demo=error"              1440,900
shot d2-agent-detail-attention      "/d2/agent/agent-2"                  1440,900
shot d2-agent-retired               "/d2/agent/agent-4"                  1440,900
shot d2-lane-archived               "/d2/lane/lane-6"                    1440,900
shot d2-lanes-archived-view         "/d2/lanes/archived"                 1440,900
shot d2-lane-disconnected-vm        "/d2/lane/lane-10"                   1440,900

# Direction 3 — Adaptive Expert Workspace
shot d3-supervision-workspace       "/d3/workspace"                      1440,900
shot d3-runtime-targets             "/d3/runtime"                        1440,900

# Narrow-window behavior
shot narrow-d1-focused-lane         "/d1/lane/lane-1"                    760,1000
shot narrow-d2-agent-roster         "/d2/agents"                         760,1000
