#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: verify-packaged-lease-stop-failure-fence.sh" >&2
  exit 64
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(unset CDPATH; cd -- "$script_dir/.." && pwd -P)
runtime="$repo_root/crates/codegen/grokptah-agent-bridge/src/computer_use/macos_isolated_runtime.rs"
handoff="$script_dir/COMPUTER_USE_PACKAGED_LEASE_STOP_FAILURE_HANDOFF.md"
bundle="/private/tmp/grokptah-packaged-lease-stop-failure-40730e4-v1.bundle"
bundle_sha="c42d6166567c1af78f8d99302051249d0777ec916aae6da26946c028f6ace44a"

require_text() {
  needle=$1
  file=$2
  if ! grep -F "$needle" "$file" >/dev/null; then
    echo "missing stop-failure fence marker: $needle" >&2
    exit 65
  fi
}

require_text 'fn finish_terminal_stop<' "$runtime"
require_text 'fn packaged_guest_is_acquirable(' "$runtime"
require_text 'self.lease = None;' "$runtime"
require_text 'isolated packaged guest is stopping, failed, or already exited' "$runtime"
require_text 'Candidate source commit: `40730e4`' "$handoff"
require_text 'cannot resume input or admit a replacement agent' "$handoff"

candidate=$(git -C "$repo_root" rev-parse --verify '40730e4^{commit}')
if [ "$candidate" != "40730e48ce96d077997874acc940ce91ca6497bb" ]; then
  echo "unexpected stop-failure candidate: $candidate" >&2
  exit 66
fi

if [ -e "$bundle" ]; then
  observed_bundle_sha=$(shasum -a 256 "$bundle" | awk '{print $1}')
  if [ "$observed_bundle_sha" != "$bundle_sha" ]; then
    echo "stop-failure bundle SHA-256 mismatch" >&2
    exit 67
  fi
  git -C "$repo_root" bundle verify "$bundle" >/dev/null
  bundle_status=verified
else
  bundle_status=not_present
fi

printf 'packaged_lease_stop_failure_source=present\n'
printf 'packaged_lease_stop_failure_candidate=40730e4\n'
printf 'packaged_lease_stop_failure_claim_status=source_only\n'
printf 'packaged_lease_stop_failure_bundle=%s\n' "$bundle_status"
