//! Native host-sentinel runner — Linux fail-closed + verifier nonclaims.
//!
//! Linux CI proves the exclusive native path never falls back to synthetic
//! probes or upgrades PASS/admission. Live Mac collection is a separate worker.

use grokptah_isolated_surface::{
    isolated_surface_admission_available, parse_sep18_checklist_run_args, run_sep18_checklist,
    seal_native_host_sentinel_pack, seal_vf_dry_run_pack, verify_evidence_pack,
    EvidenceVerifierCode, HostSentinelProbeKind, HostSentinelSnapshot, IsolatedSurfaceHarness,
    NativeSentinelEvidence, NativeSentinelRunnerOutcome, NativeSentinelRunnerPlatform,
    PhysicalProofMarkers, ProofEvidenceClass, Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate,
    Sep18NoModelProofSequencer, StopEvidence, VfDryRunEvidence, VfDryRunOutcome, VfDryRunPlatform,
    NATIVE_HOST_SENTINEL_NONCLAIM,
};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn default_harness_does_not_attach_native_collector() {
    let harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    assert!(!harness.native_host_collector_attached());
    assert!(!isolated_surface_admission_available());
}

#[test]
fn compare_only_refresh_allowed_without_native_attach() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness
        .refresh_host_sentinels(HostSentinelSnapshot::synthetic_baseline())
        .expect("compare-only is allowed before native attach");
    assert!(!harness.native_host_collector_attached());
}

#[cfg(target_os = "macos")]
#[test]
fn attach_after_probe_is_rejected() {
    use grokptah_isolated_surface::MacHostSentinelCollector;

    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("synthetic boot probes first");
    let collector = MacHostSentinelCollector::new("/explicit/checkout");
    let err = harness
        .attach_native_collector(collector)
        .expect_err("attach after probe");
    assert!(err
        .message
        .contains("native collector must be attached before any host sentinel probe"));
}

#[cfg(target_os = "macos")]
#[test]
fn compare_only_forbidden_after_native_attach() {
    use grokptah_isolated_surface::MacHostSentinelCollector;

    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    let collector = MacHostSentinelCollector::new("/explicit/checkout");
    harness
        .attach_native_collector(collector)
        .expect("attach before probe");
    let err = harness
        .refresh_host_sentinels(HostSentinelSnapshot::synthetic_baseline())
        .expect_err("compare-only after attach");
    assert!(err
        .message
        .contains("compare-only snapshot refresh is forbidden after native collector attachment"));
}

#[test]
fn explicit_checkout_required_by_cli_and_runner() {
    let err = parse_sep18_checklist_run_args(&args(&["--native-host-sentinels", "--vf-dry-run"]))
        .expect_err("checkout required");
    assert!(err.message.contains("--checkout"));

    let err = parse_sep18_checklist_run_args(&args(&["--native-host-sentinels"]))
        .expect_err("vf-dry-run required");
    assert!(err.message.contains("--vf-dry-run"));

    let err = parse_sep18_checklist_run_args(&args(&[
        "--native-host-sentinels",
        "--vf-dry-run",
        "--checkout",
        "",
    ]))
    .expect_err("empty checkout");
    assert!(err.message.contains("checkout"));

    let mut config = Sep18ChecklistRunnerConfig::contained_browser_default();
    config.substrate = Sep18ChecklistSubstrate::NativeHostSentinel;
    config.checkout_path = None;
    let outcome = run_sep18_checklist(HostSentinelSnapshot::synthetic_baseline(), config);
    let err = outcome.runner_error.expect("explicit checkout required");
    assert!(err.message.contains("explicitly supplied checkout PATH"));
    let native = outcome
        .pack
        .native_host_sentinel
        .as_ref()
        .expect("native nested");
    #[cfg(target_os = "macos")]
    {
        assert_eq!(native.platform, NativeSentinelRunnerPlatform::MacOs);
        assert_ne!(
            native.outcome,
            NativeSentinelRunnerOutcome::UnsupportedPlatform
        );
        assert!(!native.synthetic_fallback_used);
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(native.platform, NativeSentinelRunnerPlatform::NonMacOs);
        assert_eq!(
            native.outcome,
            NativeSentinelRunnerOutcome::UnsupportedPlatform
        );
    }
}

#[test]
fn native_mode_requires_vf_dry_run_and_explicit_checkout() {
    let request = parse_sep18_checklist_run_args(&args(&[
        "--native-host-sentinels",
        "--vf-dry-run",
        "--checkout",
        "/explicit/checkout",
        "-o",
        "native-vf.json",
    ]))
    .expect("native+VF+checkout");
    assert_eq!(
        request.config.substrate,
        Sep18ChecklistSubstrate::NativeHostSentinel
    );
    assert_eq!(
        request.config.checkout_path.as_deref(),
        Some(std::path::Path::new("/explicit/checkout"))
    );
    assert!(request.config.vf_physical_mac_proof_id.is_some());
    assert_eq!(request.output, std::path::PathBuf::from("native-vf.json"));
}

#[test]
fn native_vf_stop_summary_is_not_hardcoded_zero() {
    let stop = StopEvidence {
        surface_id: "vf-stop".into(),
        channels_destroyed: 2,
        host_sentinels_unchanged: true,
        host_sentinel_probes_performed: 4,
        live_host_sentinel_collection: false,
        host_sentinel_probe_error: None,
        last_host_sentinel_probe_kind: Some(HostSentinelProbeKind::NativeMacHost),
        backend_fence_error: None,
        persist_snapshot_error: None,
        backend_destroy_error: None,
        disposition: None,
    };
    let evidence = VfDryRunEvidence::unsupported(
        VfDryRunPlatform::MacOsDryRun,
        VfDryRunOutcome::BackendUnavailable,
    )
    .with_native_requested()
    .with_stop_summary(&stop);

    let pack = seal_vf_dry_run_pack(evidence.clone());
    assert_eq!(pack.host_sentinel_probes.probes_performed, 4);
    assert_eq!(pack.host_sentinel_probes.channels_destroyed, 2);
    assert!(pack.host_sentinel_probes.host_sentinels_unchanged_at_stop);
    assert_eq!(
        pack.host_sentinel_probes.last_probe_kind,
        Some(HostSentinelProbeKind::NativeMacHost)
    );
    let nested = pack.vf_dry_run.as_ref().expect("vf nested");
    assert_eq!(nested.host_sentinel_probes_performed, 4);
    assert_eq!(nested.channels_destroyed, 2);
    assert_eq!(
        nested.last_host_sentinel_probe_kind,
        Some(HostSentinelProbeKind::NativeMacHost)
    );

    let native_outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
    );
    assert!(
        native_outcome.runner_error.is_none(),
        "{:?}",
        native_outcome.runner_error
    );
    let vf = native_outcome
        .pack
        .vf_dry_run
        .as_ref()
        .expect("native+VF nested Stop summary");
    assert!(vf.native_host_sentinels_requested);
    let native = native_outcome
        .pack
        .native_host_sentinel
        .as_ref()
        .expect("native nested")
        .vf_dry_run
        .as_ref()
        .expect("native carries VF evidence");
    assert_eq!(vf, native);
}

#[test]
fn macos_fail_closed_pack_is_not_labeled_non_macos() {
    let evidence = NativeSentinelEvidence::macos_fail_closed(
        NativeSentinelRunnerOutcome::BackendUnavailable,
        "TCC / collector unavailable",
    );
    let pack = seal_native_host_sentinel_pack(evidence);
    let native = pack.native_host_sentinel.as_ref().expect("native nested");
    assert_eq!(native.platform, NativeSentinelRunnerPlatform::MacOs);
    assert_eq!(
        native.outcome,
        NativeSentinelRunnerOutcome::BackendUnavailable
    );
    assert_ne!(
        native.outcome,
        NativeSentinelRunnerOutcome::UnsupportedPlatform
    );
    assert!(!native.synthetic_fallback_used);
    assert!(!pack.physical_pass_claimed);
    assert!(!pack.vf_pass_claimed);
    assert!(!pack.admission_available);
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
}

#[test]
fn sequencer_native_runner_is_fail_closed_on_linux_ci() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer
        .run_native_host_sentinel("/explicit/checkout")
        .expect("native runner artifact");
    assert!(!evidence.synthetic_fallback_used);
    assert!(!evidence.physical_pass_claimed);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert_eq!(evidence.evidence_class, ProofEvidenceClass::Synthetic);
    assert_eq!(evidence.nonclaim, NATIVE_HOST_SENTINEL_NONCLAIM);
    assert!(!isolated_surface_admission_available());
    assert!(evidence.vf_dry_run.is_some());

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
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
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
    let config = Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout")
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
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
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
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
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
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
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
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
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
    let evidence = sequencer
        .run_native_host_sentinel("/explicit/checkout")
        .expect("artifact");
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

fn native_pack_with_forged_completed_seal() -> grokptah_isolated_surface::Sep18EvidencePack {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let mut sealed = sequencer.run_happy_path().expect("synthetic sealed stop");
    sealed.stop_evidence.live_host_sentinel_collection = true;
    sealed.stop_evidence.last_host_sentinel_probe_kind = Some(HostSentinelProbeKind::NativeMacHost);
    if sealed.stop_evidence.host_sentinel_probes_performed == 0 {
        sealed.stop_evidence.host_sentinel_probes_performed = 1;
    }
    if sealed.stop_evidence.channels_destroyed == 0 {
        sealed.stop_evidence.channels_destroyed = 1;
    }
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::native_host_sentinel("/explicit/checkout"),
    );
    let mut pack = outcome.pack;
    pack.sealed_evidence = Some(sealed.clone());
    pack.checklist_completed = true;
    pack.host_sentinel_probes =
        grokptah_isolated_surface::HostSentinelProbeSummary::from_stop_evidence(
            &sealed.stop_evidence,
            0,
        );
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.platform = NativeSentinelRunnerPlatform::MacOs;
        native.outcome = NativeSentinelRunnerOutcome::NativeProbesCompleted;
        native.checklist_completed = true;
        native.live_host_sentinel_collection = true;
        native.independent_collection_verified = true;
        native.post_stop_inject_fenced = true;
        native.channels_destroyed = sealed.stop_evidence.channels_destroyed;
        native.host_sentinel_probes_performed = sealed.stop_evidence.host_sentinel_probes_performed;
        native.last_host_sentinel_probe_kind = Some(HostSentinelProbeKind::NativeMacHost);
        native.sealed_evidence = Some(sealed);
        native.synthetic_fallback_used = false;
    }
    pack
}

#[test]
fn forged_live_with_synthetic_kind_is_rejected() {
    let mut pack = native_pack_with_forged_completed_seal();
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.live_host_sentinel_collection = true;
        native.independent_collection_verified = true;
        native.last_host_sentinel_probe_kind = Some(HostSentinelProbeKind::SyntheticRehearsal);
        if let Some(sealed) = native.sealed_evidence.as_mut() {
            sealed.stop_evidence.live_host_sentinel_collection = true;
            sealed.stop_evidence.last_host_sentinel_probe_kind =
                Some(HostSentinelProbeKind::SyntheticRehearsal);
        }
    }
    if let Some(sealed) = pack.sealed_evidence.as_mut() {
        sealed.stop_evidence.live_host_sentinel_collection = true;
        sealed.stop_evidence.last_host_sentinel_probe_kind =
            Some(HostSentinelProbeKind::SyntheticRehearsal);
    }
    pack.host_sentinel_probes
        .live_host_sentinel_collection_at_stop = true;
    pack.host_sentinel_probes.last_probe_kind = Some(HostSentinelProbeKind::SyntheticRehearsal);

    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::NativeSentinelSyntheticFallback
    );
}

#[test]
fn forged_live_with_missing_kind_is_rejected() {
    let mut pack = native_pack_with_forged_completed_seal();
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.live_host_sentinel_collection = true;
        native.independent_collection_verified = true;
        native.last_host_sentinel_probe_kind = None;
        if let Some(sealed) = native.sealed_evidence.as_mut() {
            sealed.stop_evidence.live_host_sentinel_collection = true;
            sealed.stop_evidence.last_host_sentinel_probe_kind = None;
        }
    }
    if let Some(sealed) = pack.sealed_evidence.as_mut() {
        sealed.stop_evidence.live_host_sentinel_collection = true;
        sealed.stop_evidence.last_host_sentinel_probe_kind = None;
    }
    pack.host_sentinel_probes
        .live_host_sentinel_collection_at_stop = true;
    pack.host_sentinel_probes.last_probe_kind = None;

    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::HostSentinelProbeSummaryMismatch
    );
}

#[test]
fn forged_count_mismatch_is_rejected() {
    let mut pack = native_pack_with_forged_completed_seal();
    pack.host_sentinel_probes.probes_performed = 99;
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.host_sentinel_probes_performed = 99;
    }
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::HostSentinelProbeSummaryMismatch
    );
}

#[test]
fn flipped_independent_flag_is_rejected() {
    let mut pack = native_pack_with_forged_completed_seal();
    if let Some(native) = pack.native_host_sentinel.as_mut() {
        native.independent_collection_verified = true;
        native.live_host_sentinel_collection = false;
    }
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert!(!matches!(decision.code, EvidenceVerifierCode::Accepted));
}
