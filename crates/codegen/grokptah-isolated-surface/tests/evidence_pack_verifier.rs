//! Independent evidence-pack verifier tests — tamper detection + dry-run nonclaims.

#[cfg(not(feature = "browser-engine"))]
mod cb_happy_path {
    use grokptah_isolated_surface::{
        run_sep18_checklist, seal_contained_browser_dry_run_pack, verify_evidence_pack,
        EvidenceVerifierCode, GuestLifecycleDisposition, HostSentinelSnapshot, ProofEvidenceClass,
        Sep18ChecklistRunnerConfig, Sep18NoModelProofSequencer,
    };

    #[test]
    fn cb_dry_run_pack_verifies_via_runner() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        assert!(outcome.runner_error.is_none(), "{:?}", outcome.runner_error);
        let decision = verify_evidence_pack(&outcome.pack);
        assert!(decision.accepted, "{:?}", decision);
        assert!(!outcome.pack.physical_pass_claimed);
        assert!(!outcome.pack.admission_available);
    }

    #[test]
    fn tampered_evidence_class_is_rejected() {
        let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
        let evidence = sequencer
            .run_contained_browser_dry_run()
            .expect("cb dry-run");
        let mut pack = seal_contained_browser_dry_run_pack(evidence);
        pack.evidence_class = ProofEvidenceClass::VirtualizationFramework;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::EvidenceClassTampered);
    }

    #[test]
    fn dual_field_evidence_class_upgrade_on_cb_is_rejected() {
        let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
        let evidence = sequencer
            .run_contained_browser_dry_run()
            .expect("cb dry-run");
        let mut pack = seal_contained_browser_dry_run_pack(evidence);
        pack.evidence_class = ProofEvidenceClass::VirtualizationFramework;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.evidence_class = ProofEvidenceClass::VirtualizationFramework;
        }
        if let Some(cb) = pack.contained_browser.as_mut() {
            cb.evidence_class = ProofEvidenceClass::VirtualizationFramework;
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::EvidenceClassTampered);
    }

    #[test]
    fn cb_dry_run_with_forged_markers_and_pass_claims_is_rejected() {
        use grokptah_isolated_surface::PhysicalProofMarkers;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.physical_proof_markers = PhysicalProofMarkers {
            mac_worker_attested: true,
            live_host_sentinel_collection: true,
            physical_mac_proof_id: Some("forged-mac-proof".into()),
        };
        pack.vf_pass_claimed = true;
        pack.physical_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::PhysicalPassWithoutMacMarkers
        );
    }

    #[test]
    fn tampered_physical_pass_claim_is_rejected() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.physical_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::PhysicalPassWithoutMacMarkers
        );
    }

    #[test]
    fn tampered_stop_teardown_omission_is_rejected() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.checklist_steps.retain(|step| {
                !matches!(
                    step,
                    grokptah_isolated_surface::ChecklistStep::StopDestroyed
                )
            });
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::ChecklistIncomplete);
    }

    #[test]
    fn stop_teardown_required_even_when_checklist_not_completed() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.checklist_completed = false;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.checklist_steps.retain(|step| {
                !matches!(
                    step,
                    grokptah_isolated_surface::ChecklistStep::StopDestroyed
                )
            });
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::ChecklistIncomplete);
    }

    #[test]
    fn tampered_open_channels_is_rejected() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.host_sentinel_probes.channels_open_after_stop = 2;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::StopChannelsStillOpen);
    }

    #[test]
    fn tampered_admission_true_is_rejected() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.admission_available = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::AdmissionMustStayFalse);
    }

    #[test]
    fn uncertain_downgrade_after_possible_inject_is_rejected() {
        use grokptah_isolated_surface::FaultMatrixCase;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject
        );
    }

    #[test]
    fn uncertain_downgrade_with_cleared_fault_matrix_is_rejected() {
        use grokptah_isolated_surface::FaultMatrixCase;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
        }
        pack.fault_matrix_case = None;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject
        );
    }
}

#[test]
fn vf_dry_run_pack_never_qualifies_physical_pass_on_linux() {
    use grokptah_isolated_surface::{
        run_sep18_checklist, verify_evidence_pack, EvidenceVerifierCode, HostSentinelSnapshot,
        Sep18ChecklistRunnerConfig,
    };

    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::vf_dry_run("linux-ci-vf-dry-run"),
    );
    assert!(outcome.runner_error.is_none());
    assert!(!outcome.pack.physical_pass_claimed);
    assert!(!outcome.pack.vf_pass_claimed);

    let decision = verify_evidence_pack(&outcome.pack);
    assert!(decision.accepted, "{:?}", decision);

    let mut tampered = outcome.pack;
    tampered.vf_pass_claimed = true;
    tampered.physical_pass_claimed = true;
    let decision = verify_evidence_pack(&tampered);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::PhysicalPassWithoutMacMarkers
    );
}

#[test]
fn verify_cli_roundtrip_json() {
    use grokptah_isolated_surface::{
        parse_evidence_pack, run_sep18_checklist, serialize_evidence_pack, verify_evidence_pack,
        HostSentinelSnapshot, Sep18ChecklistRunnerConfig,
    };

    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::contained_browser_default(),
    );
    let json = serialize_evidence_pack(&outcome.pack).expect("serialize");
    let parsed = parse_evidence_pack(&json).expect("parse");
    let decision = verify_evidence_pack(&parsed);
    assert!(decision.accepted, "{:?}", decision);
}
