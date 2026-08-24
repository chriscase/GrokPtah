#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: verify-packaged-lease-fence.sh" >&2
  exit 64
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(unset CDPATH; cd -- "$script_dir/.." && pwd -P)
guest="$repo_root/crates/codegen/grokptah-agent-bridge/src/computer_use/isolated_guest.rs"
runtime="$repo_root/crates/codegen/grokptah-agent-bridge/src/computer_use/macos_isolated_runtime.rs"
handoff="$script_dir/COMPUTER_USE_PACKAGED_LEASE_EXTERNAL_HANDOFF.md"
bundle="/private/tmp/grokptah-packaged-lease-b250b70-v2.bundle"
bundle_sha="4d4f46a85168b45476c1acc47ba7e289bfcb27b6ea08b173d862a038f27a2352"

require_text() {
  needle=$1
  file=$2
  if ! grep -F "$needle" "$file" >/dev/null; then
    echo "missing lease-fence source marker: $needle" >&2
    exit 65
  fi
}

require_text 'pub(crate) fn issue(' "$guest"
require_text 'pub(crate) fn require(' "$guest"
require_text 'lease: Option<IsolatedGuestLease>' "$runtime"
require_text 'pub(crate) fn acquire(' "$runtime"
require_text 'pub(crate) fn start(' "$runtime"
require_text 'pub(crate) fn read_frame(' "$runtime"
require_text 'pub(crate) fn write_input(' "$runtime"
require_text 'pub(crate) fn stop(' "$runtime"
require_text 'fn require_lease(' "$runtime"
require_text 'isolated packaged guest cleanup requires the lease to be revoked' "$runtime"
require_text 'self.lease = None;' "$runtime"
require_text 'Candidate head: `b250b70`' "$handoff"
require_text 'no packaged VM or hardware claim' "$handoff"

candidate=$(git -C "$repo_root" rev-parse --verify 'b250b70^{commit}')
if [ "$candidate" != "b250b7096c6131721864b39f4cc5fdce5e3ada15" ]; then
  echo "unexpected packaged lease-fence candidate: $candidate" >&2
  exit 66
fi

if [ -e "$bundle" ]; then
  observed_bundle_sha=$(shasum -a 256 "$bundle" | awk '{print $1}')
  if [ "$observed_bundle_sha" != "$bundle_sha" ]; then
    echo "packaged lease-fence bundle SHA-256 mismatch" >&2
    exit 67
  fi
  git -C "$repo_root" bundle verify "$bundle" >/dev/null
  bundle_status=verified
else
  bundle_status=not_present
fi

printf 'packaged_lease_fence_source=present\n'
printf 'packaged_lease_fence_candidate=b250b70\n'
printf 'packaged_lease_fence_claim_status=source_only\n'
printf 'packaged_lease_fence_bundle=%s\n' "$bundle_status"
