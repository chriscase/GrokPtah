//! Sep 18 no-model proof sequencer + backend SPI regression tests.

use grokptah_isolated_surface::{
    ChecklistStep, ContainedBrowserBackend, FaultMatrixCase, HarnessErrorCode,
    HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness, ProofEvidenceClass,
    Sep18NoModelProofSequencer,
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
fn contained_browser_stub_fails_closed() {
    let mut backend = ContainedBrowserBackend::new();
    assert_eq!(
        backend.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );
    let err = backend.boot().expect_err("unsupported");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(!backend.is_booted());
}

#[test]
fn harness_with_contained_browser_backend_fails_on_boot() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    );
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );
    let err = harness.boot().expect_err("boot must fail closed");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
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
