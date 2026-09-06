//! Bridge integration for the Isolated Surface Proof Harness (#288/#286).
//!
//! These tests prove the harness is reachable from the agent bridge workspace
//! and that admission remains fail-closed. They do not open a VM or claim
//! Virtualization.framework qualification.

use grokptah_agent_bridge::computer_use::{
    computer_use_isolated_surface_admission, ContainedBrowserBackend, GuestLifecyclePhase,
    HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness, ProofEvidenceClass,
    Sep18NoModelProofSequencer, SyntheticGuestAction, VfDryRunOutcome, VfDryRunPlatform,
    VfLaunchReceipt, SYNTHETIC_HARNESS_NONCLAIM, VF_DRY_RUN_NONCLAIM,
};

#[test]
fn admission_is_fail_closed_from_bridge() {
    assert!(!computer_use_isolated_surface_admission());
}

#[test]
fn bridge_can_run_synthetic_canonical_proof() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    assert_eq!(harness.evidence_class(), ProofEvidenceClass::Synthetic);
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

#[test]
fn bridge_sep18_sequencer_reachable() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer.run_happy_path().expect("sequencer");
    assert_eq!(sealed.evidence_class, ProofEvidenceClass::Synthetic);
}

#[test]
fn bridge_contained_browser_stub_honest_label() {
    let backend = ContainedBrowserBackend::new();
    assert_eq!(
        backend.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );
}

#[test]
fn bridge_vf_dry_run_honest_nonclaim() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer
        .run_vf_dry_run(VfLaunchReceipt {
            physical_mac_proof_id: "bridge-dry-run".into(),
        })
        .expect("dry-run");
    match evidence.platform {
        VfDryRunPlatform::NonMacOs => {
            assert_eq!(evidence.outcome, VfDryRunOutcome::UnsupportedPlatform);
        }
        VfDryRunPlatform::MacOsFeatureDisabled => {
            assert_eq!(evidence.outcome, VfDryRunOutcome::FeatureDisabled);
        }
        VfDryRunPlatform::MacOsDryRun => {
            panic!("vf-backend dry-run is not exercised by this bridge integration test");
        }
    }
    assert!(!evidence.physical_pass_claimed);
    assert_eq!(evidence.nonclaim, VF_DRY_RUN_NONCLAIM);
    assert!(evidence.nonclaim.contains("never claim Sep 18 PASS"));
    assert!(!computer_use_isolated_surface_admission());
}
