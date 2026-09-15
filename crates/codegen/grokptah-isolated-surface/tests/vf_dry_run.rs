//! VF dry-run verifier bindings — live-at-stop kind and dry-run markers.
//!
//! These are synthetic fixtures for the independent pack verifier. They do not
//! claim macOS, TCC, VF, or physical qualification.

use grokptah_isolated_surface::{
    seal_vf_dry_run_pack, verify_evidence_pack, EvidenceVerifierCode, HostSentinelProbeKind,
    PhysicalProofMarkers, VfDryRunEvidence, VfDryRunOutcome, VfDryRunPlatform,
};

fn vf_live_native_fixture() -> VfDryRunEvidence {
    let mut evidence = VfDryRunEvidence::fail_closed(
        VfDryRunPlatform::MacOsDryRun,
        VfDryRunOutcome::BackendUnavailable,
    );
    evidence.native_host_sentinels_requested = true;
    evidence.host_sentinel_probes_performed = 3;
    evidence.live_host_sentinel_collection_at_stop = true;
    evidence.host_sentinels_unchanged_at_stop = true;
    evidence.channels_destroyed = 1;
    evidence.channels_open_after_stop = 0;
    evidence.last_host_sentinel_probe_kind = Some(HostSentinelProbeKind::NativeMacHost);
    evidence
}

#[test]
fn vf_dry_run_live_at_stop_native_kind_with_dry_run_none_is_accepted() {
    let pack = seal_vf_dry_run_pack(vf_live_native_fixture());
    assert_eq!(
        pack.physical_proof_markers,
        PhysicalProofMarkers::dry_run_none()
    );
    assert_eq!(
        pack.host_sentinel_probes.last_probe_kind,
        Some(HostSentinelProbeKind::NativeMacHost)
    );
    assert!(
        pack.host_sentinel_probes
            .live_host_sentinel_collection_at_stop
    );
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
    assert!(!pack.physical_pass_claimed);
    assert!(!pack.vf_pass_claimed);
    assert!(!pack.admission_available);
}

#[test]
fn vf_dry_run_live_at_stop_wrong_kind_is_rejected() {
    let mut evidence = vf_live_native_fixture();
    evidence.last_host_sentinel_probe_kind = Some(HostSentinelProbeKind::SyntheticRehearsal);
    let pack = seal_vf_dry_run_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::NativeSentinelSyntheticFallback
    );
}

#[test]
fn vf_dry_run_live_at_stop_missing_kind_is_rejected() {
    let mut evidence = vf_live_native_fixture();
    evidence.last_host_sentinel_probe_kind = None;
    let pack = seal_vf_dry_run_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::HostSentinelProbeSummaryMismatch
    );
}

#[test]
fn vf_dry_run_non_empty_physical_markers_are_rejected() {
    let mut pack = seal_vf_dry_run_pack(vf_live_native_fixture());
    pack.physical_proof_markers = PhysicalProofMarkers {
        mac_worker_attested: true,
        live_host_sentinel_collection: true,
        physical_mac_proof_id: Some("forged-vf-dry-run-markers".into()),
    };
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
    );
}

#[test]
fn vf_dry_run_marker_only_live_claim_is_rejected() {
    let evidence = VfDryRunEvidence::fail_closed(
        VfDryRunPlatform::NonMacOs,
        VfDryRunOutcome::UnsupportedPlatform,
    );
    let mut pack = seal_vf_dry_run_pack(evidence);
    pack.physical_proof_markers = PhysicalProofMarkers {
        mac_worker_attested: false,
        live_host_sentinel_collection: true,
        physical_mac_proof_id: None,
    };
    assert!(
        !pack
            .host_sentinel_probes
            .live_host_sentinel_collection_at_stop
    );
    assert!(pack.host_sentinel_probes.last_probe_kind.is_none());
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
    );
}
