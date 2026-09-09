//! Native host-sentinel runner — Linux fail-closed + verifier nonclaims.
//!
//! Linux CI proves the exclusive native path never falls back to synthetic
//! probes or upgrades PASS/admission. Live Mac collection is a separate worker.

use grokptah_isolated_surface::{
    isolated_surface_admission_available, run_sep18_checklist, seal_native_host_sentinel_pack,
    verify_evidence_pack, EvidenceVerifierCode, HostSentinelSnapshot, IsolatedSurfaceHarness,
    NativeSentinelRunnerOutcome, NativeSentinelRunnerPlatform, PhysicalProofMarkers,
    ProofEvidenceClass, Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate,
    Sep18NoModelProofSequencer, NATIVE_HOST_SENTINEL_NONCLAIM,
};

#[test]
fn default_harness_does_not_attach_native_collector() {
    let harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    assert!(!harness.native_host_collector_attached());
    assert!(!isolated_surface_admission_available());
}

#[test]
fn sequencer_native_runner_is_fail_closed_on_linux_ci() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer
        .run_native_host_sentinel(".")
        .expect("native runner artifact");
    assert!(!evidence.synthetic_fallback_used);
    assert!(!evidence.physical_pass_claimed);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert_eq!(evidence.evidence_class, ProofEvidenceClass::Synthetic);
    assert_eq!(evidence.nonclaim, NATIVE_HOST_SENTINEL_NONCLAIM);
    assert!(!isolated_surface_admission_available());

    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(evidence.platform, NativeSentinelRunnerPlatform::NonMacOs);
        assert_eq!(
            evidence.outcome,
            NativeSentinelRunnerOutcome::UnsupportedPlatform
        );
        assert!(!evidence.live_host_sentinel_collection);
        assert!(!evidence.independent_collection_verified);
        assert!(!evidence.checklist_completed);
        assert!(evidence.sealed_evidence.is_none());
    }
}

#[test]
fn runner_native_pack_verifies_without_pass_or_admission() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("."),
    );
    assert!(outcome.runner_error.is_none(), "{:?}", outcome.runner_error);
    assert_eq!(
        outcome.pack.substrate,
        Sep18ChecklistSubstrate::NativeHostSentinel
    );
    assert!(!outcome.pack.physical_pass_claimed);
    assert!(!outcome.pack.isolation_pass_claimed);
    assert!(!outcome.pack.vf_pass_claimed);
    assert!(!outcome.pack.admission_available);
    assert_eq!(
        outcome.pack.physical_proof_markers,
        PhysicalProofMarkers::dry_run_none()
    );
    let decision = verify_evidence_pack(&outcome.pack);
    assert!(decision.accepted, "{decision:?}");
    assert!(!isolated_surface_admission_available());
}

#[test]
fn native_runner_fault_matrix_is_rejected() {
    let config = Sep18ChecklistRunnerConfig::native_host_sentinel(".")
        .with_fault_matrix(grokptah_isolated_surface::FaultMatrixCase::BootStop);
    let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), config);
    assert!(outcome.runner_error.is_some());
    assert!(!outcome.pack.admission_available);
    assert!(!outcome.pack.physical_pass_claimed);
}

#[test]
fn forged_native_live_collection_on_linux_pack_is_rejected() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("."),
    );
    let mut pack = outcome.pack;
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.live_host_sentinel_collection = true;
    }
    pack.host_sentinel_probes
        .live_host_sentinel_collection_at_stop = true;
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::NativeSentinelLiveClaimOnUnsupportedPlatform
    );
}

#[test]
fn native_synthetic_fallback_is_rejected() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("."),
    );
    let mut pack = outcome.pack;
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.synthetic_fallback_used = true;
    }
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::NativeSentinelSyntheticFallback
    );
}

#[test]
fn native_physical_markers_upgrade_is_rejected() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("."),
    );
    let mut pack = outcome.pack;
    pack.physical_proof_markers = PhysicalProofMarkers {
        mac_worker_attested: true,
        live_host_sentinel_collection: true,
        physical_mac_proof_id: Some("forged-native-pass".into()),
    };
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::NativeSentinelCannotQualifyPhysicalPass
    );
}

#[test]
fn native_pass_claim_is_rejected() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("."),
    );
    let mut pack = outcome.pack;
    pack.physical_pass_claimed = true;
    pack.vf_pass_claimed = true;
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert!(!matches!(decision.code, EvidenceVerifierCode::Accepted));
}

#[test]
fn default_cb_runner_still_does_not_use_native_substrate() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::contained_browser_default(),
    );
    assert_ne!(
        outcome.pack.substrate,
        Sep18ChecklistSubstrate::NativeHostSentinel
    );
    assert!(outcome.pack.native_host_sentinel.is_none());
    assert!(!outcome.pack.admission_available);
}

#[test]
fn seal_helper_matches_runner_pack_shape() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer.run_native_host_sentinel(".").expect("artifact");
    let pack = seal_native_host_sentinel_pack(evidence);
    assert_eq!(pack.substrate, Sep18ChecklistSubstrate::NativeHostSentinel);
    assert_eq!(pack.nonclaim, NATIVE_HOST_SENTINEL_NONCLAIM);
    assert_eq!(
        pack.physical_proof_markers,
        PhysicalProofMarkers::dry_run_none()
    );
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
}
