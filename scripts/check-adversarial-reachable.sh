#!/bin/sh
# Verify the Computer Use adversarial suites are actually reachable from CI.
#
# Why this exists: `grokptah-isolated-visual` declares its own `[workspace]`,
# so it is NOT built by the grokptah-agent-bridge or desktop `cargo test`
# invocations even though the bridge path-depends on it. Its library is
# compiled as a dependency, but its 23 unit tests and 5 adversarial-matrix
# tests never execute unless a workflow targets that manifest directly. A test
# file can also stop running silently (renamed, `autotests = false`, or moved
# behind a feature) without any compile error, so presence on disk is not
# evidence that CI runs it.
#
# Proof classes are labeled and NOT interchangeable:
#   [dynamic] the test binary was compiled and the harness enumerated the name.
#   [static]  the file and its invocation path were inspected, nothing was run.
#
# Usage: scripts/check-adversarial-reachable.sh
# Exit:  0 = every expected suite reachable; 1 = at least one is not.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$root"

iso="crates/codegen/grokptah-isolated-visual/Cargo.toml"
bridge_tests="crates/codegen/grokptah-agent-bridge/tests"
status=0

# --- [dynamic] isolated-visual: enumerate real, compiled test names ----------
listing=$(mktemp)
trap 'rm -f "$listing"' EXIT

cargo test --locked --manifest-path "$iso" --lib -- --list 2>/dev/null \
  | sed 's/: test$//' > "$listing"
cargo test --locked --manifest-path "$iso" --test adversarial_matrix -- --list 2>/dev/null \
  | sed 's/: test$//' >> "$listing"

# Safety/authority invariants that must never silently stop being exercised.
expected='
cleanup::tests::incomplete_observation_is_uncertain
git_hermetic::tests::rejects_alternates_replace_index_hooks_and_unpinned_git
host_tests::crash_after_inject_then_two_restarts_do_not_replay
host_tests::duplicate_dispatch_is_exactly_once
host_tests::lifecycle_create_ready_running_closing_and_no_resume
host_tests::one_agent_per_guest_and_forged_identities_denied
host_tests::preflight_is_ineligible_for_vm_qualification
host_tests::public_projection_redacts_secrets_and_omits_frame_bytes
host_tests::resolver_object_substitution_and_rename_fail_closed
ids::tests::relative_paths_reject_traversal_case_and_unicode
lease::tests::clock_rollback_does_not_mutate
lifecycle::tests::phases_are_forward_only_and_terminal_blocks_resume
preflight::tests::missing_artifacts_fail_closed
resolver::tests::rejects_symlink_and_traversal_inputs
cleanup_failure_is_uncertain_and_guest_is_not_cleaned
corrupted_and_legacy_records_fail_closed
forged_conflict_domain_cannot_steal_capacity
store_lock_rejects_a_second_open
symlink_and_traversal_in_source_tree_are_denied
'

for name in $expected; do
  if grep -qxF "$name" "$listing"; then
    printf '[dynamic] ok   %s\n' "$name"
  else
    printf '[dynamic] MISS %s (not enumerated by the compiled harness)\n' "$name"
    status=1
  fi
done

# --- [static] bridge suites: reachable via the bridge-wide `cargo test` ------
# These run today under .github/workflows/desktop.yml, which invokes a bare
# `cargo test --locked` in the bridge directory. That sweeps every autodiscovered
# target, so the checks here are that discovery is still on and the files exist.
if grep -q 'autotests *= *false' crates/codegen/grokptah-agent-bridge/Cargo.toml; then
  printf '[static]  FAIL bridge sets autotests=false; tests/ is no longer autodiscovered\n'
  status=1
else
  printf '[static]  ok   bridge test autodiscovery enabled\n'
fi

for suite in orchestration_adversarial computer_use_release_gate isolation_capability; do
  if [ -f "$bridge_tests/$suite.rs" ]; then
    printf '[static]  ok   %s.rs present (swept by the bridge-wide cargo test)\n' "$suite"
  else
    printf '[static]  FAIL %s.rs missing\n' "$suite"
    status=1
  fi
done

if grep -q 'grokptah-isolated-visual' .github/workflows/desktop.yml; then
  printf '[static]  ok   desktop.yml gates changes under grokptah-isolated-visual\n'
else
  printf '[static]  FAIL desktop.yml does not react to grokptah-isolated-visual changes\n'
  status=1
fi

if [ "$status" -ne 0 ]; then
  printf '\nadversarial reachability check FAILED\n' >&2
else
  printf '\nall expected adversarial suites reachable\n'
fi

exit "$status"
