#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: verify-isolated-runtime-evidence.sh" >&2
  exit 64
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(unset CDPATH; cd -- "$script_dir/.." && pwd -P)
evidence="$script_dir/COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md"
handoff="$script_dir/COMPUTER_USE_ISOLATED_GROK_BUILD_HANDOFF.md"

# shellcheck disable=SC2016 # sed expressions intentionally contain literal backticks
source_head=$(sed -n 's/^- Source head at evidence cutoff: `\([0-9a-f]\{40\}\)`.*/\1/p' "$evidence")
# shellcheck disable=SC2016 # sed expressions intentionally contain literal backticks
bundle=$(sed -n 's/^- Bundle: `\([^`]*\)`.*/\1/p' "$evidence" | head -n 1)
# shellcheck disable=SC2016 # sed expressions intentionally contain literal backticks
expected_sha=$(sed -n 's/^- Bundle SHA-256: `\([0-9a-f]\{64\}\)`.*/\1/p' "$evidence" | head -n 1)
# shellcheck disable=SC2016 # sed expressions intentionally contain literal backticks
handoff_source_head=$(sed -n 's/^- Source cutoff: `\([0-9a-f]\{40\}\)`.*/\1/p' "$handoff" | head -n 1)
# shellcheck disable=SC2016 # sed expressions intentionally contain literal backticks
handoff_bundle=$(sed -n 's/^- Source bundle: `\([^`]*\)`.*/\1/p' "$handoff" | head -n 1)
# shellcheck disable=SC2016 # sed expressions intentionally contain literal backticks
handoff_sha=$(sed -n 's/^- Bundle SHA-256: `\([0-9a-f]\{64\}\)`.*/\1/p' "$handoff" | head -n 1)

if [ -z "$source_head" ] || [ -z "$bundle" ] || [ -z "$expected_sha" ] \
  || [ -z "$handoff_source_head" ] || [ -z "$handoff_bundle" ] || [ -z "$handoff_sha" ]; then
  echo "evidence identity is incomplete" >&2
  exit 65
fi
if [ "$handoff_source_head" != "$source_head" ] \
  || [ "$handoff_bundle" != "$bundle" ] \
  || [ "$handoff_sha" != "$expected_sha" ]; then
  echo "handoff and evidence identities disagree" >&2
  exit 65
fi
git -C "$repo_root" cat-file -e "${source_head}^{commit}"
if [ ! -f "$bundle" ] || [ -L "$bundle" ]; then
  echo "evidence bundle is not a regular file: $bundle" >&2
  exit 66
fi
observed_sha=$(shasum -a 256 "$bundle" | awk '{print $1}')
if [ "$observed_sha" != "$expected_sha" ]; then
  echo "evidence bundle SHA-256 mismatch" >&2
  exit 65
fi
git -C "$repo_root" bundle verify "$bundle" >/dev/null
if ! git -C "$repo_root" bundle list-heads "$bundle" \
  | awk '{print $1}' \
  | grep -Fx "$source_head" >/dev/null; then
  echo "evidence source head is not the bundle head" >&2
  exit 65
fi
grep -F 'Status: source-level progress only' "$evidence" >/dev/null
grep -F 'does not qualify a packaged' "$evidence" >/dev/null
grep -F 'no signed app has launched it' "$evidence" >/dev/null
grep -F 'boot/render/input/cleanup result is claimed' "$evidence" >/dev/null
grep -F 'Status: **qualification procedure only; no VM capability is claimed' "$handoff" >/dev/null
grep -F 'report NOT QUALIFIED' "$handoff" >/dev/null

printf 'evidence_source_head=%s\n' "$source_head"
printf 'evidence_bundle=%s\n' "$bundle"
printf 'evidence_bundle_sha256=%s\n' "$observed_sha"
printf 'evidence_bundle_history=complete\n'
printf 'evidence_claim_status=source_only\n'
