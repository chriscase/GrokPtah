#!/bin/sh
# Verify the Computer Use adversarial suites are actually reachable from CI.
#
# Why this exists: `grokptah-isolated-visual` declares its own `[workspace]`, so
# it is NOT built by the grokptah-agent-bridge or desktop `cargo test`
# invocations even though the bridge path-depends on it. Its library compiles as
# a dependency, but its unit tests and adversarial matrix never execute unless a
# workflow targets that manifest directly. A test can also stop running silently
# -- renamed, `autotests = false`, or moved behind a feature -- without any
# compile error, so presence on disk is not evidence that CI runs it.
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
bridge="crates/codegen/grokptah-agent-bridge"
status=0

# --- [dynamic] isolated-visual: enumerate real, compiled test names ----------
listing=$(mktemp)
trap 'rm -f "$listing"' EXIT

cargo test --locked --manifest-path "$iso" --lib -- --list 2>/dev/null \
  | sed 's/: test$//' > "$listing"
cargo test --locked --manifest-path "$iso" --test adversarial_matrix -- --list 2>/dev/null \
  | sed 's/: test$//' >> "$listing"

# Safety and authority invariants that must never silently stop being exercised.
# Each name maps to a reviewed P0; losing one loses the proof for that P0.
expected='
cleanup::tests::a_fabricated_exact_receipt_does_not_validate
cleanup::tests::a_probe_that_cannot_run_leaves_cleanup_unresolved
cleanup::tests::a_resource_still_present_is_surfaced_not_swallowed
cleanup::tests::an_omitted_resource_cannot_pass_by_silence
cleanup::tests::an_unknown_resource_never_counts_as_released
code_identity::tests::failed_verification_is_never_promoted
code_identity::tests::negated_values_cannot_invert_into_a_positive_class
code_identity::tests::probe_is_unavailable_off_macos_and_refuses_to_guess
code_identity::tests::prose_containing_authority_text_does_not_classify
git_hermetic::tests::rejects_alternates_replace_index_hooks_and_unpinned_git
host_tests::a_failed_overlay_deletion_is_uncertain_and_the_guest_is_not_cleaned
host_tests::an_expired_lease_is_reaped_and_cannot_dispatch
host_tests::crash_after_inject_then_two_restarts_do_not_replay
host_tests::duplicate_dispatch_id_with_a_changed_payload_is_a_conflict
host_tests::duplicate_dispatch_is_exactly_once
host_tests::forged_conflict_domain_cannot_steal_capacity
host_tests::lifecycle_create_ready_running_closing_and_no_resume
host_tests::one_agent_per_guest_and_forged_identities_denied
host_tests::preflight_is_ineligible_for_vm_qualification
host_tests::public_projection_redacts_secrets_and_omits_frame_bytes
host_tests::resolver_rejects_absent_objects_length_lies_and_renames
host_tests::store_lock_rejects_a_second_open
ids::tests::relative_paths_reject_traversal_case_and_unicode
lease::tests::clock_rollback_does_not_mutate
lifecycle::tests::phases_are_forward_only_and_terminal_blocks_resume
occupancy::tests::a_corrupt_record_denies_rather_than_reading_as_clear
occupancy::tests::a_semantically_invalid_record_denies
occupancy::tests::two_live_owners_cannot_share_a_resource
packaged_authority::tests::a_missing_os_requirement_is_never_synthesized
packaged_authority::tests::a_planted_codesign_text_file_cannot_change_the_verdict
packaged_authority::tests::a_requirement_that_merely_mentions_the_identifier_is_refused
packaged_authority::tests::a_synthesized_requirement_for_the_observed_team_is_refused
packaged_authority::tests::an_unavailable_probe_cannot_admit
packaged_authority::tests::symlinked_entitlements_fail_closed_instead_of_defaulting
preflight::tests::a_forged_artifact_root_cannot_supply_its_own_expectations
preflight::tests::missing_everything_fails_closed
resolver::tests::rejects_symlink_and_traversal_inputs
trust_root::tests::requirement_for_another_bundle_is_refused
trust_root::tests::trust_root_inside_the_artifact_root_is_refused
a_bundle_local_attestation_file_is_recorded_and_never_read
a_duplicate_dispatch_id_with_a_changed_payload_is_refused
a_fabricated_cleanup_receipt_does_not_validate
a_host_opened_without_artifacts_never_claims_virtualization_framework
a_second_agent_cannot_take_a_leased_guest
a_second_process_cannot_open_the_same_store
a_stale_lease_is_reaped_on_open
a_torn_lease_record_is_quarantined_not_trusted
a_truncated_lease_record_is_quarantined
a_trust_root_inside_the_artifact_root_is_refused
cleanup_that_leaves_a_resource_behind_is_uncertain
forged_artifact_root_cannot_supply_its_own_expectations
negated_signing_text_cannot_invert_into_admission
production_preflight_is_reachable_and_denies_on_this_host
rust_and_js_digests_agree
symlinked_entitlements_fail_closed
synthesized_designated_requirement_and_team_identity_are_refused
two_restarts_after_injection_never_replay
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
# These run under .github/workflows/desktop.yml, which invokes a bare
# `cargo test --locked` in the bridge directory. That sweeps every
# autodiscovered target, so the checks here are that discovery is still on and
# the files exist. Nothing is executed by this script.
if grep -q 'autotests *= *false' "$bridge/Cargo.toml"; then
  printf '[static]  FAIL bridge sets autotests=false; tests/ is no longer autodiscovered\n'
  status=1
else
  printf '[static]  ok   bridge test autodiscovery enabled\n'
fi

for suite in orchestration_adversarial computer_use_release_gate isolation_capability; do
  if [ -f "$bridge/tests/$suite.rs" ]; then
    printf '[static]  ok   %s.rs present (swept by the bridge-wide cargo test)\n' "$suite"
  else
    printf '[static]  FAIL %s.rs missing\n' "$suite"
    status=1
  fi
done

# The single-authority rule: the bridge must not grow a second lease/dispatch
# state machine beside the one in grokptah-isolated-visual.
if [ -f "$bridge/src/computer_use/helper_authority.rs" ]; then
  printf '[static]  FAIL a second helper-local authority reappeared at helper_authority.rs\n'
  status=1
else
  printf '[static]  ok   no second helper-local lease/dispatch authority\n'
fi

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
