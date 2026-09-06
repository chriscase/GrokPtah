//! Sep 18 no-model proof sequencer + backend SPI regression tests.

use grokptah_isolated_surface::{
    ChecklistStep, ContainedBrowserBackend, FaultMatrixCase, HarnessErrorCode,
    HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness, ProofEvidenceClass,
    Sep18NoModelProofSequencer, VfDryRunOutcome, VfDryRunPlatform, VfLaunchReceipt,
    VF_DRY_RUN_NONCLAIM,
};
#[cfg(not(feature = "browser-engine"))]
use grokptah_isolated_surface::{
    ContainedBrowserDryRunOutcome, ContainedBrowserDryRunPlatform,
    CONTAINED_BROWSER_DRY_RUN_NONCLAIM,
};
use tempfile::TempDir;

#[test]
fn synthetic_backend_implements_spi() {
    let mut backend = grokptah_isolated_surface::SyntheticGuest::new();
    assert_eq!(backend.evidence_class(), ProofEvidenceClass::Synthetic);
    let frame = backend.boot().expect("boot");
    assert_eq!(frame.epoch, 1);
    let observed = backend.observe_frame().expect("frame");
    assert_eq!(observed.epoch, 1);
    backend.destroy().expect("destroy");
    assert!(!backend.is_booted());
}

#[test]
fn contained_browser_substrate_honest_label() {
    let backend = ContainedBrowserBackend::new();
    assert_eq!(
        backend.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );
    assert!(!backend.isolation_proof_available());
    assert!(!backend.evidence_class().is_vf_qualification_eligible());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_simulator_boot_succeeds() {
    let mut backend = ContainedBrowserBackend::new();
    let frame = backend.boot().expect("simulator boot");
    assert_eq!(frame.epoch, 1);
    assert!(backend.is_booted());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn harness_with_contained_browser_backend_runs_checklist() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser is permitted");
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );

    harness.boot().expect("boot");
    let before = harness.observe_frame().expect("frame");
    assert!(before.epoch > 0);
    let delta = harness
        .inject_guest_action(grokptah_isolated_surface::GuestLocalAction::ClickGuestButton)
        .expect("inject");
    assert!(delta.guest_local_change);
    let evidence = harness.stop().expect("stop");
    assert_eq!(
        evidence.disposition,
        Some(grokptah_isolated_surface::GuestLifecycleDisposition::Stopped)
    );
}

#[test]
fn contained_browser_stop_tears_down_after_fence_error() {
    grokptah_isolated_surface::run_contained_browser_stop_fence_regression(
        HostSentinelSnapshot::synthetic_baseline(),
    )
    .expect("fence regression");
}

#[test]
fn stop_fence_first_error_still_tears_down_channels() {
    use grokptah_isolated_surface::{FaultInjectingBackend, GuestLifecyclePhase};

    let mut wrapped = FaultInjectingBackend::new(grokptah_isolated_surface::SyntheticGuest::new());
    wrapped.fail_stop_fence = true;

    let mut harness =
        IsolatedSurfaceHarness::with_backend(HostSentinelSnapshot::synthetic_baseline(), wrapped)
            .expect("synthetic permitted");
    harness.boot().expect("boot");
    assert_eq!(harness.channels().open_count(), 2);

    let evidence = harness.stop().expect("stop completes teardown");
    assert!(evidence.backend_fence_error.is_some());
    assert_eq!(evidence.channels_destroyed, 2);
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    harness.channels().assert_all_destroyed().expect("no leak");
}

#[test]
fn inject_backend_err_marks_uncertain_before_stop() {
    use grokptah_isolated_surface::GuestLifecycleDisposition;

    let mut wrapped = grokptah_isolated_surface::FaultInjectingBackend::new(
        grokptah_isolated_surface::SyntheticGuest::new(),
    );
    wrapped.fail_inject_with_err = true;

    let mut harness =
        IsolatedSurfaceHarness::with_backend(HostSentinelSnapshot::synthetic_baseline(), wrapped)
            .expect("synthetic permitted");
    harness.boot().expect("boot");
    harness
        .inject_guest_action(grokptah_isolated_surface::GuestLocalAction::ClickGuestButton)
        .expect_err("inject err");

    assert_eq!(
        harness.lifecycle().disposition,
        Some(GuestLifecycleDisposition::Uncertain)
    );
    let evidence = harness.stop().expect("stop");
    assert_eq!(
        evidence.disposition,
        Some(GuestLifecycleDisposition::Uncertain)
    );
}

#[test]
fn with_backend_rejects_vf_without_receipt() {
    struct FakeVfBackend;

    impl IsolatedSurfaceBackend for FakeVfBackend {
        fn evidence_class(&self) -> ProofEvidenceClass {
            ProofEvidenceClass::VirtualizationFramework
        }
        fn boot(
            &mut self,
        ) -> grokptah_isolated_surface::HarnessResult<grokptah_isolated_surface::GuestFrame>
        {
            Err(grokptah_isolated_surface::HarnessError::backend_unavailable("stub"))
        }
        fn observe_frame(
            &self,
        ) -> grokptah_isolated_surface::HarnessResult<grokptah_isolated_surface::GuestFrame>
        {
            Err(grokptah_isolated_surface::HarnessError::backend_unavailable("stub"))
        }
        fn inject_guest_local(
            &mut self,
            _action: grokptah_isolated_surface::GuestLocalAction,
        ) -> grokptah_isolated_surface::HarnessResult<grokptah_isolated_surface::InjectOutcome>
        {
            Err(grokptah_isolated_surface::HarnessError::backend_unavailable("stub"))
        }
        fn destroy(&mut self) -> grokptah_isolated_surface::HarnessResult<()> {
            Ok(())
        }
        fn is_booted(&self) -> bool {
            false
        }
    }

    let err = match IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        FakeVfBackend,
    ) {
        Err(err) => err,
        Ok(_) => panic!("vf without receipt must be rejected"),
    };
    assert_eq!(err.code, HarnessErrorCode::InvalidState);
}

#[test]
fn sep18_happy_path_seals_synthetic_evidence() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer.run_happy_path().expect("happy path");
    assert_eq!(sealed.evidence_class, ProofEvidenceClass::Synthetic);
    assert!(sealed
        .checklist_steps
        .contains(&ChecklistStep::EvidenceSealed));
    assert!(sealed.stop_evidence.host_sentinels_unchanged);
    assert!(sealed.nonclaim.contains("ineligible"));
    assert!(sealed.fault_matrix_case.is_none());
}

#[test]
fn sep18_fault_matrix_boot_stop() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer
        .run_fault_matrix(FaultMatrixCase::BootStop)
        .expect("boot stop");
    assert_eq!(sealed.fault_matrix_case, Some(FaultMatrixCase::BootStop));
    assert_eq!(sealed.evidence_class, ProofEvidenceClass::Synthetic);
}

#[test]
fn sep18_fault_matrix_pre_dispatch_stop() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer
        .run_fault_matrix(FaultMatrixCase::PreDispatchStop)
        .expect("pre-dispatch stop");
    assert_eq!(
        sealed.fault_matrix_case,
        Some(FaultMatrixCase::PreDispatchStop)
    );
}

#[test]
fn sep18_fault_matrix_lost_ack_uncertain() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer
        .run_fault_matrix(FaultMatrixCase::LostAckUncertain)
        .expect("lost ack");
    assert_eq!(
        sealed.fault_matrix_case,
        Some(FaultMatrixCase::LostAckUncertain)
    );
}

#[test]
fn sep18_fault_matrix_restart_no_replay() {
    let dir = TempDir::new().expect("tempdir");
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline())
        .with_snapshot_root(dir.path());
    let sealed = sequencer
        .run_fault_matrix(FaultMatrixCase::RestartNoReplay)
        .expect("restart no replay");
    assert_eq!(
        sealed.fault_matrix_case,
        Some(FaultMatrixCase::RestartNoReplay)
    );
    assert!(sealed
        .checklist_steps
        .contains(&ChecklistStep::StaleTokensRejected));
    assert!(
        sealed.stop_evidence.channels_destroyed > 0
            || sealed.stop_evidence.disposition
                == Some(grokptah_isolated_surface::GuestLifecycleDisposition::Uncertain)
    );
}

#[test]
fn main_checkout_sentinel_regression() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("boot");
    harness
        .host_probe_mut()
        .simulate_host_mutation("main_checkout");
    let evidence = harness.stop().expect("stop despite probe failure");
    assert!(!evidence.host_sentinels_unchanged);
    let probe_err = evidence.host_sentinel_probe_error.expect("probe error");
    assert_eq!(probe_err.code, HarnessErrorCode::HostSentinelViolation);
    assert!(probe_err.message.contains("main checkout"));
}

#[test]
fn evidence_class_never_upgrades_at_seal() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer.run_happy_path().expect("seal");
    assert!(!sealed.evidence_class.is_vf_qualification_eligible());
}

#[test]
fn sep18_vf_dry_run_unsupported_on_linux_ci() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let receipt = VfLaunchReceipt {
        physical_mac_proof_id: "dry-run-linux-ci".into(),
    };
    let evidence = sequencer.run_vf_dry_run(receipt).expect("dry-run artifact");
    assert_eq!(evidence.platform, VfDryRunPlatform::NonMacOs);
    assert_eq!(evidence.outcome, VfDryRunOutcome::UnsupportedPlatform);
    assert!(!evidence.physical_pass_claimed);
    assert!(!evidence.boot_attempted);
    assert!(evidence.nonclaim.contains("never claim Sep 18 PASS"));
    assert_eq!(evidence.nonclaim, VF_DRY_RUN_NONCLAIM);
}

#[test]
fn vf_dry_run_rejects_empty_receipt() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let err = sequencer
        .run_vf_dry_run(VfLaunchReceipt {
            physical_mac_proof_id: "  ".into(),
        })
        .expect_err("empty receipt");
    assert_eq!(err.code, HarnessErrorCode::InvalidState);
}

#[test]
fn harness_refresh_host_sentinels_uses_probe_path() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("boot");
    harness
        .refresh_host_sentinels(HostSentinelSnapshot::synthetic_baseline())
        .expect("refresh matches baseline");
    assert!(harness.sentinels().verified_via_probe());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn sep18_contained_browser_dry_run_simulator_on_linux_ci() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer
        .run_contained_browser_dry_run()
        .expect("contained browser dry-run");
    assert_eq!(
        evidence.platform,
        ContainedBrowserDryRunPlatform::SimulatorSubstrate
    );
    assert_eq!(
        evidence.outcome,
        ContainedBrowserDryRunOutcome::SubstrateRehearsal
    );
    assert_eq!(
        evidence.evidence_class,
        ProofEvidenceClass::ContainedBrowser
    );
    assert!(evidence.checklist_completed);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert_eq!(evidence.nonclaim, CONTAINED_BROWSER_DRY_RUN_NONCLAIM);
    assert!(evidence.nonclaim.contains("not isolation PASS"));
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());

    let sealed = evidence.sealed_evidence.expect("sealed checklist");
    assert_eq!(sealed.evidence_class, ProofEvidenceClass::ContainedBrowser);
    assert!(!sealed.evidence_class.is_vf_qualification_eligible());
    assert!(sealed
        .checklist_steps
        .contains(&ChecklistStep::EvidenceSealed));
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn sep18_contained_browser_fault_matrix_boot_stop() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer
        .run_contained_browser_fault_matrix(FaultMatrixCase::BootStop)
        .expect("boot stop");
    assert_eq!(sealed.fault_matrix_case, Some(FaultMatrixCase::BootStop));
    assert_eq!(sealed.evidence_class, ProofEvidenceClass::ContainedBrowser);
    assert!(!sealed.evidence_class.is_vf_qualification_eligible());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn sep18_contained_browser_fault_matrix_lost_ack_uncertain() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer
        .run_contained_browser_fault_matrix(FaultMatrixCase::LostAckUncertain)
        .expect("lost ack");
    assert_eq!(
        sealed.fault_matrix_case,
        Some(FaultMatrixCase::LostAckUncertain)
    );
    assert_eq!(
        sealed.stop_evidence.disposition,
        Some(grokptah_isolated_surface::GuestLifecycleDisposition::Uncertain)
    );
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_no_pass_laundering_at_seal() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer.run_contained_browser_dry_run().expect("dry-run");
    let sealed = evidence.sealed_evidence.expect("sealed");
    assert_ne!(
        sealed.evidence_class,
        ProofEvidenceClass::VirtualizationFramework
    );
    assert_ne!(sealed.evidence_class, ProofEvidenceClass::Synthetic);
    assert!(!sealed.evidence_class.is_vf_qualification_eligible());
}
