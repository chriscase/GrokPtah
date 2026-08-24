#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: verify-packaged-lease-report.sh REPORT" >&2
  exit 64
fi

report=$1
if [ ! -f "$report" ]; then
  echo "report is not a regular file: $report" >&2
  exit 65
fi

require_text() {
  needle=$1
  label=$2
  if ! grep -F -- "$needle" "$report" >/dev/null; then
    echo "missing packaged-lease evidence: $label ($needle)" >&2
    exit 66
  fi
}

require_regex() {
  pattern=$1
  label=$2
  if ! grep -Eiq -- "$pattern" "$report"; then
    echo "missing packaged-lease evidence: $label" >&2
    exit 67
  fi
}

# Identity and immutability: a report for another checkout is not evidence for
# this campaign, even if its tests happen to be green.
require_text "b250b7096c6131721864b39f4cc5fdce5e3ada15" "candidate commit"
require_text "295a4ff62939af1a3034119653c83c7a0a2e1bff" "candidate parent"
require_text "5919e3343af20a78e17459b8ac8454bbc5aeca7e" "PR head"
require_text "4d4f46a85168b45476c1acc47ba7e289bfcb27b6ea08b173d862a038f27a2352" "input bundle SHA-256"

# Reproducibility and resource isolation must be visible in the external
# transcript, not inferred from the final test result.
require_text "RUSTC_WRAPPER=/opt/homebrew/bin/sccache" "sccache wrapper"
require_text "SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache" "namespaced sccache directory"
require_text "CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default" "shared target"
require_regex "disk|headroom" "disk-headroom report"
require_regex "target" "target ownership/size report"
require_regex "lsof|open[[:space:]-]+handle" "open-handle report"

# The exact source checks from the handoff must have run. A generic claim that
# tests passed is insufficient because it may have exercised only the simulator.
require_text "rustfmt --edition 2021 --check" "rustfmt check"
require_text "cargo metadata --locked --offline --no-deps" "metadata check"
require_text "cargo test --locked" "locked test invocation"
require_regex "isolated_guest" "isolated guest test"
require_regex "native_launch_descriptor_set_must_be_complete_and_unique" "macOS supervisor test"

# Keep the claim boundary explicit: this gate is source-level only.
require_regex "source[- ]only|no packaged VM|not packaged" "source-only boundary"

# Accept common report labels, but reject a labeled failure even if another
# line contains the word PASS (for example, a passing prerequisite).
if grep -Eiq -- "(decision|result|status|qualification)[[:space:]:=]+(not[[:space:]-]+qualified|fail(ed|ure)?)" "$report"; then
  echo "packaged-lease report is NOT QUALIFIED" >&2
  exit 68
fi
if ! grep -Eiq -- "(decision|result|status|qualification)[[:space:]:=]+pass|^[[:space:]]*PASS([[:space:]]|$)" "$report"; then
  echo "missing final PASS decision" >&2
  exit 69
fi

printf 'packaged_lease_report=present\n'
printf 'packaged_lease_report_candidate=b250b70\n'
printf 'packaged_lease_report_claim_status=source_only\n'
