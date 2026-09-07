//! Independent evidence-pack verifier tests — tamper detection + dry-run nonclaims.

#[cfg(not(feature = "browser-engine"))]
mod cb_happy_path {
    use grokptah_isolated_surface::{
        run_sep18_checklist, seal_contained_browser_dry_run_pack, verify_evidence_pack,
        EvidenceVerifierCode, GuestLifecycleDisposition, HostSentinelSnapshot, ProofEvidenceClass,
        Sep18ChecklistRunnerConfig, Sep18NoModelProofSequencer,
    };

    fn sync_cb_sealed(pack: &mut grokptah_isolated_surface::Sep18EvidencePack) {
        if let Some(cb) = pack.contained_browser.as_mut() {
            if let Some(sealed) = &pack.sealed_evidence {
                cb.sealed_evidence = Some(sealed.clone());
            }
        }
    }

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
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::VfPassClaimOnDryRun);
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
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
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
        sync_cb_sealed(&mut pack);
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
        sync_cb_sealed(&mut pack);
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
        sync_cb_sealed(&mut pack);
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
        sync_cb_sealed(&mut pack);
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject
        );
    }

    #[test]
    fn uncertain_fabricated_postcondition_with_cleared_fault_is_rejected() {
        use grokptah_isolated_surface::{ChecklistStep, FaultMatrixCase};

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            if !sealed
                .checklist_steps
                .contains(&ChecklistStep::PostconditionVerified)
            {
                sealed
                    .checklist_steps
                    .push(ChecklistStep::PostconditionVerified);
            }
        }
        pack.fault_matrix_case = None;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::PackSealedEvidenceMismatch
        );
    }

    #[test]
    fn uncertain_pack_sealed_fabricated_vs_nested_cb_honest_is_rejected() {
        use grokptah_isolated_surface::FaultMatrixCase;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        let honest_cb_sealed = pack
            .contained_browser
            .as_ref()
            .and_then(|cb| cb.sealed_evidence.clone())
            .expect("cb sealed");
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
        }
        pack.fault_matrix_case = None;
        if let Some(cb) = pack.contained_browser.as_mut() {
            cb.sealed_evidence = Some(honest_cb_sealed);
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::PackSealedEvidenceMismatch
        );
    }

    #[test]
    fn uncertain_cleared_trigger_steps_with_stopped_is_rejected() {
        use grokptah_isolated_surface::{ChecklistStep, FaultMatrixCase};

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        let tampered_sealed = {
            let sealed = pack.sealed_evidence.as_mut().expect("sealed");
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            sealed.checklist_steps.retain(|step| {
                !matches!(
                    step,
                    ChecklistStep::GuestLocalActionMarkedPossible
                        | ChecklistStep::PostconditionVerified
                )
            });
            sealed.clone()
        };
        pack.sealed_evidence = Some(tampered_sealed.clone());
        if let Some(cb) = pack.contained_browser.as_mut() {
            cb.sealed_evidence = Some(tampered_sealed);
        }
        pack.fault_matrix_case = None;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::ChecklistIncomplete);
    }

    #[test]
    fn fault_matrix_pack_vs_sealed_mismatch_is_rejected() {
        use grokptah_isolated_surface::{ChecklistStep, FaultMatrixCase};

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        pack.fault_matrix_case = Some(FaultMatrixCase::LostAckUncertain);
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            if !sealed
                .checklist_steps
                .contains(&ChecklistStep::PostconditionVerified)
            {
                sealed
                    .checklist_steps
                    .push(ChecklistStep::PostconditionVerified);
            }
        }
        if let Some(cb) = pack.contained_browser.as_mut() {
            if let Some(cb_sealed) = cb.sealed_evidence.as_mut() {
                *cb_sealed = pack.sealed_evidence.clone().expect("sealed");
            }
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::PackSealedEvidenceMismatch
        );
    }

    #[test]
    fn stop_teardown_omitted_even_when_booted_dropped_is_rejected() {
        use grokptah_isolated_surface::ChecklistStep;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.checklist_steps.retain(|step| {
                !matches!(step, ChecklistStep::Booted | ChecklistStep::StopDestroyed)
            });
        }
        if let Some(cb) = pack.contained_browser.as_mut() {
            if let Some(cb_sealed) = cb.sealed_evidence.as_mut() {
                *cb_sealed = pack.sealed_evidence.clone().expect("sealed");
            }
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::ChecklistIncomplete);
    }

    #[test]
    fn stop_destroyed_with_zero_channels_destroyed_is_rejected() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.stop_evidence.channels_destroyed = 0;
        }
        pack.host_sentinel_probes.channels_destroyed = 0;
        if let Some(cb) = pack.contained_browser.as_mut() {
            if let Some(cb_sealed) = cb.sealed_evidence.as_mut() {
                *cb_sealed = pack.sealed_evidence.clone().expect("sealed");
            }
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::StopChannelsStillOpen);
    }

    #[test]
    fn host_sentinel_probe_count_mismatch_uses_honest_code() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.host_sentinel_probes.probes_performed = 99;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch
        );
    }

    fn forged_markers() -> grokptah_isolated_surface::PhysicalProofMarkers {
        grokptah_isolated_surface::PhysicalProofMarkers {
            mac_worker_attested: true,
            live_host_sentinel_collection: true,
            physical_mac_proof_id: Some("linux-ci-self-attest".into()),
        }
    }

    #[test]
    fn vf_dry_run_dropped_nested_with_markers_and_pass_is_rejected() {
        use grokptah_isolated_surface::PhysicalProofMarkers;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::vf_dry_run("linux-ci-vf-dry-run"),
        );
        let mut pack = outcome.pack;
        pack.vf_dry_run = None;
        pack.physical_proof_markers = PhysicalProofMarkers {
            mac_worker_attested: true,
            live_host_sentinel_collection: true,
            physical_mac_proof_id: Some("linux-ci-self-attest".into()),
        };
        pack.vf_pass_claimed = true;
        pack.physical_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
        );
    }

    #[test]
    fn synthetic_markers_and_pass_claims_are_rejected() {
        use grokptah_isolated_surface::{
            run_sep18_checklist, Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate,
        };

        let mut config = Sep18ChecklistRunnerConfig::contained_browser_default();
        config.substrate = Sep18ChecklistSubstrate::SyntheticHarness;
        let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), config);
        assert!(outcome.runner_error.is_none());
        let mut pack = outcome.pack;
        pack.physical_proof_markers = forged_markers();
        pack.vf_pass_claimed = true;
        pack.physical_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
        );
    }

    #[test]
    fn cb_relabel_to_vf_dry_run_with_markers_and_pass_is_rejected() {
        use grokptah_isolated_surface::{
            ProofEvidenceClass, Sep18ChecklistSubstrate, VF_DRY_RUN_NONCLAIM,
        };

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.substrate = Sep18ChecklistSubstrate::VfDryRun;
        pack.evidence_class = ProofEvidenceClass::VirtualizationFramework;
        pack.contained_browser = None;
        pack.vf_dry_run = None;
        pack.nonclaim = VF_DRY_RUN_NONCLAIM.into();
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.evidence_class = ProofEvidenceClass::VirtualizationFramework;
            sealed.nonclaim = VF_DRY_RUN_NONCLAIM.into();
        }
        pack.physical_proof_markers = forged_markers();
        pack.vf_pass_claimed = true;
        pack.physical_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
        );
    }

    #[test]
    fn cb_relabel_to_synthetic_with_markers_and_pass_is_rejected() {
        use grokptah_isolated_surface::{
            ProofEvidenceClass, Sep18ChecklistSubstrate, SYNTHETIC_HARNESS_NONCLAIM,
        };

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default(),
        );
        let mut pack = outcome.pack;
        pack.substrate = Sep18ChecklistSubstrate::SyntheticHarness;
        pack.evidence_class = ProofEvidenceClass::Synthetic;
        pack.contained_browser = None;
        pack.nonclaim = SYNTHETIC_HARNESS_NONCLAIM.into();
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.evidence_class = ProofEvidenceClass::Synthetic;
            sealed.nonclaim = SYNTHETIC_HARNESS_NONCLAIM.into();
        }
        pack.physical_proof_markers = forged_markers();
        pack.vf_pass_claimed = true;
        pack.physical_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
        );
    }

    fn sync_both_sealed(
        pack: &mut grokptah_isolated_surface::Sep18EvidencePack,
        tamper: impl FnOnce(&mut grokptah_isolated_surface::SealedProofEvidence),
    ) {
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            tamper(sealed);
            let synced = sealed.clone();
            if let Some(cb) = pack.contained_browser.as_mut() {
                cb.sealed_evidence = Some(synced);
            }
        }
    }

    #[test]
    fn uncertain_both_copies_fabricated_postcondition_stopped_is_rejected() {
        use grokptah_isolated_surface::{
            ChecklistStep, FaultMatrixCase, GuestLifecycleDisposition,
        };

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        sync_both_sealed(&mut pack, |sealed| {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            if !sealed
                .checklist_steps
                .contains(&ChecklistStep::PostconditionVerified)
            {
                sealed
                    .checklist_steps
                    .push(ChecklistStep::PostconditionVerified);
            }
        });
        pack.fault_matrix_case = None;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject
        );
    }

    #[test]
    fn uncertain_drop_nested_cb_sealed_while_pack_fabricated_is_rejected() {
        use grokptah_isolated_surface::{
            ChecklistStep, FaultMatrixCase, GuestLifecycleDisposition,
        };

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        sync_both_sealed(&mut pack, |sealed| {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            if !sealed
                .checklist_steps
                .contains(&ChecklistStep::PostconditionVerified)
            {
                sealed
                    .checklist_steps
                    .push(ChecklistStep::PostconditionVerified);
            }
        });
        pack.fault_matrix_case = None;
        if let Some(cb) = pack.contained_browser.as_mut() {
            cb.sealed_evidence = None;
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::SubstrateNestedEvidenceMissing
        );
    }

    #[test]
    fn uncertain_relabel_boot_stop_with_scrubbed_triggers_is_rejected() {
        use grokptah_isolated_surface::{
            ChecklistStep, FaultMatrixCase, GuestLifecycleDisposition,
        };

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        sync_both_sealed(&mut pack, |sealed| {
            sealed.fault_matrix_case = Some(FaultMatrixCase::BootStop);
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            sealed.checklist_steps.retain(|step| {
                !matches!(
                    step,
                    ChecklistStep::GuestLocalActionMarkedPossible | ChecklistStep::FrameChallenge
                )
            });
        });
        pack.fault_matrix_case = Some(FaultMatrixCase::BootStop);
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::FaultMatrixDispositionMismatch
        );
    }

    #[test]
    fn synthetic_lost_ack_fabricated_postcondition_stopped_is_rejected() {
        use grokptah_isolated_surface::{
            ChecklistStep, FaultMatrixCase, GuestLifecycleDisposition, Sep18ChecklistSubstrate,
        };

        let mut config = Sep18ChecklistRunnerConfig::contained_browser_default();
        config.substrate = Sep18ChecklistSubstrate::SyntheticHarness;
        config = config.with_fault_matrix(FaultMatrixCase::LostAckUncertain);
        let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), config);
        assert!(outcome.runner_error.is_none());
        let mut pack = outcome.pack;
        if let Some(sealed) = pack.sealed_evidence.as_mut() {
            sealed.fault_matrix_case = None;
            sealed.stop_evidence.disposition = Some(GuestLifecycleDisposition::Stopped);
            if !sealed
                .checklist_steps
                .contains(&ChecklistStep::PostconditionVerified)
            {
                sealed
                    .checklist_steps
                    .push(ChecklistStep::PostconditionVerified);
            }
        }
        pack.fault_matrix_case = None;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject
        );
    }

    #[test]
    fn sealed_evidence_omitted_with_stop_story_is_rejected() {
        use grokptah_isolated_surface::FaultMatrixCase;

        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::contained_browser_default()
                .with_fault_matrix(FaultMatrixCase::LostAckUncertain),
        );
        let mut pack = outcome.pack;
        pack.sealed_evidence = None;
        pack.checklist_completed = false;
        if let Some(cb) = pack.contained_browser.as_mut() {
            cb.sealed_evidence = None;
        }
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(decision.code, EvidenceVerifierCode::ChecklistIncomplete);
    }

    #[test]
    fn synthetic_isolation_pass_claimed_is_rejected() {
        use grokptah_isolated_surface::{
            run_sep18_checklist, Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate,
        };

        let mut config = Sep18ChecklistRunnerConfig::contained_browser_default();
        config.substrate = Sep18ChecklistSubstrate::SyntheticHarness;
        let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), config);
        assert!(outcome.runner_error.is_none());
        let mut pack = outcome.pack;
        pack.isolation_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::IsolationPassClaimOnDryRun
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
        EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
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
