//! Bridge integration for the Isolated Surface Proof Harness (#288/#286).
//!
//! These tests prove the harness is reachable from the agent bridge workspace
//! and that admission remains fail-closed. They do not open a VM or claim
//! Virtualization.framework qualification.

use grokptah_agent_bridge::computer_use::{
    computer_use_isolated_surface_admission, GuestLifecyclePhase, HostSentinelSnapshot,
    IsolatedSurfaceHarness, ProofEvidenceClass, SyntheticGuestAction, SYNTHETIC_HARNESS_NONCLAIM,
};

#[test]
fn admission_is_fail_closed_from_bridge() {
    assert!(!computer_use_isolated_surface_admission());
}

#[test]
fn bridge_can_run_synthetic_canonical_proof() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::SyntheticHarnessIneligible
    );
    assert!(SYNTHETIC_HARNESS_NONCLAIM.contains("ineligible"));

    let evidence = harness.run_canonical_proof().expect("canonical proof");
    assert!(evidence.host_sentinels_unchanged);
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
}

#[test]
fn bridge_harness_stop_regression_smoke() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("boot");
    harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect("inject");
    harness.stop().expect("stop");
    harness.sentinels().assert_unchanged().expect("sentinels");
}
