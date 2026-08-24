#!/bin/sh
set -eu

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(unset CDPATH; cd -- "$script_dir/.." && pwd -P)

require_text() {
  needle=$1
  file=$2
  if ! grep -F -- "$needle" "$repo_root/$file" >/dev/null; then
    echo "missing qualification-doc state: $file: $needle" >&2
    exit 65
  fi
}

# The current visual lane must be the only actionable visual handoff.
require_text 'Status: **SUPERSEDED historical procedure; do not launch.**' \
  docs/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF_V13.md
require_text 'Status: **SUPERSEDED historical procedure; do not launch.**' \
  docs/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF_V14.md
require_text 'Status: **queued procedure only; no VM, package, or Computer Use capability is' \
  docs/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF_V15.md
require_text 'The earlier `b250b70` source campaign is complete' \
  docs/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF_V15.md

# Source evidence must remain explicitly narrower than packaged/live evidence.
require_text '**Decision:** **PASS — source lease-fence checks only**' \
  docs/evidence/COMPUTER_USE_PACKAGED_LEASE_B250B70_SOURCE_REPORT.md
require_text 'It does **not** prove a signed helper or guest image,' \
  docs/evidence/COMPUTER_USE_PACKAGED_LEASE_B250B70_SOURCE_REPORT.md
require_text 'The v51 public-run campaign is' docs/ROADMAP_TO_100.md
require_text '**NOT_QUALIFIED**' docs/ROADMAP_TO_100.md
require_text 'The b250 source lease-fence sub-gate is **PASS**' \
  docs/ROADMAP_TO_100.md

# Public claims must continue to reject a 100%/parity-complete inference.
require_text 'Neither document claims 100%' README.md
require_text 'This matrix does not' docs/CAPABILITY_MATRIX.md
require_text 'claim 100%.**' docs/CAPABILITY_MATRIX.md

# The coordinator is allowed to orchestrate evidence, never to mint a claim.
require_text 'no certification is claimed by' docs/OVERNIGHT_QUALIFICATION_COORDINATOR.md
require_text 'cannot claim Stage 6' docs/OVERNIGHT_QUALIFICATION_COORDINATOR.md
require_text 'Do not edit the capability matrix or claim 100%' \
  docs/OVERNIGHT_QUALIFICATION_COORDINATOR.md

printf 'qualification_doc_state=consistent\n'
printf 'current_visual_handoff=v15\n'
printf 'b250_source_evidence=pass_source_only\n'
printf 'public_100_percent_claim=forbidden\n'
