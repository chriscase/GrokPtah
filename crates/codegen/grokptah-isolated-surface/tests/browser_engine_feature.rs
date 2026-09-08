//! Compile + fail-closed regression for `--features browser-engine`.
//!
//! Run via:
//! `cargo check --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml --features browser-engine`
//! `cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml --features browser-engine --test browser_engine_feature -- --test-threads=1`

#![cfg(feature = "browser-engine")]

use grokptah_isolated_surface::{
    ContainedBrowserBackend, ContainedBrowserDryRunOutcome, ContainedBrowserDryRunPlatform,
    FaultMatrixCase, HarnessErrorCode, HostSentinelSnapshot, IsolatedSurfaceBackend,
    IsolatedSurfaceHarness, ProofEvidenceClass, Sep18NoModelProofSequencer,
    CONTAINED_BROWSER_DRY_RUN_NONCLAIM,
};

#[test]
fn browser_engine_boot_fails_closed() {
    let mut backend = ContainedBrowserBackend::new();
    assert_eq!(backend.substrate_mode_label(), "engine_unavailable");
    assert!(!backend.isolation_proof_available());
    assert_eq!(
        backend.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );
    assert!(!backend.evidence_class().is_vf_qualification_eligible());

    let err = backend.boot().expect_err("engine boot fails closed");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(!backend.is_booted());
    assert!(grokptah_isolated_surface::admit_browser_engine_capture(vec![1, 2, 3], 8, 8).is_err());
}

#[test]
fn browser_engine_dry_run_honest_nonclaim() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer
        .run_contained_browser_dry_run()
        .expect("contained browser dry-run");
    assert_eq!(
        evidence.platform,
        ContainedBrowserDryRunPlatform::BrowserEngineFeatureDisabled
    );
    assert_eq!(
        evidence.outcome,
        ContainedBrowserDryRunOutcome::BackendUnavailable
    );
    assert!(!evidence.checklist_completed);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert_eq!(evidence.nonclaim, CONTAINED_BROWSER_DRY_RUN_NONCLAIM);
    assert!(evidence.sealed_evidence.is_none());
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[test]
fn browser_engine_fault_matrix_fails_closed() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let err = sequencer
        .run_contained_browser_fault_matrix(FaultMatrixCase::BootStop)
        .expect_err("fault matrix unavailable on engine feature");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
}

#[test]
fn browser_engine_stop_teardown_after_boot_fail() {
    grokptah_isolated_surface::run_contained_browser_stop_fence_regression(
        HostSentinelSnapshot::synthetic_baseline(),
    )
    .expect("stop regression");

    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[test]
fn browser_engine_harness_boot_fail_stop_teardown() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser permitted");
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );

    let err = harness.boot().expect_err("boot fails closed");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert_eq!(harness.channels().open_count(), 2);

    let evidence = harness.stop().expect("stop always teardown");
    assert_eq!(evidence.channels_destroyed, 2);
    harness.channels().assert_all_destroyed().expect("no leak");
}
