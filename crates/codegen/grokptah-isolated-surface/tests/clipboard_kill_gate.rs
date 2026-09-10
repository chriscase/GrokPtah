//! Contained Browser clipboard isolation kill-gate — adversarial verifier +
//! Linux fail-closed harness.
//!
//! Synthetic fixtures prove the machine verifier. They cannot establish
//! physical Mac Pass. Computer Mode and admission stay false.

use grokptah_isolated_surface::{
    admit_probe_reply, clipboard_kill_gate_may_claim_pass, host_clipboard_unchanged,
    isolated_surface_admission_available, parse_sep18_checklist_run_args, run_clipboard_kill_gate,
    run_sep18_checklist, seal_clipboard_kill_gate_pack, verify_clipboard_kill_gate_evidence,
    verify_evidence_pack, ClipboardKillGateEvidence, ClipboardKillGateOutcome,
    ClipboardKillGatePlatform, ClipboardKillGateProvenance, ClipboardKillGateVerdict,
    ClipboardOperation, ClipboardProbeFailClosedReason, ClipboardWitness, ContentWorld,
    EvidenceVerifierCode, HostClipboardDigest, HostSentinelSnapshot, PageLocalClipboardReceipt,
    PhysicalProofMarkers, ProbePull, ProbeReply, ProofEvidenceClass, ScriptEvaluationPath,
    Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate, WKClipboardProbe,
    CLIPBOARD_KILL_GATE_NONCLAIM,
};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn mediated_receipts(generation: u64, epoch: u64) -> Vec<PageLocalClipboardReceipt> {
    ClipboardOperation::ALL
        .into_iter()
        .map(|operation| PageLocalClipboardReceipt {
            operation,
            generation,
            epoch,
            mediated_page_local: true,
            host_pasteboard_touched: false,
        })
        .collect()
}

fn forged_native_pass_evidence() -> ClipboardKillGateEvidence {
    let digest = HostClipboardDigest::canonical(b"stable-host-clipboard", 2);
    ClipboardKillGateEvidence {
        platform: ClipboardKillGatePlatform::MacOs,
        outcome: ClipboardKillGateOutcome::MediationProven,
        verdict: ClipboardKillGateVerdict::Pass,
        fail_closed_reason: None,
        provenance: ClipboardKillGateProvenance::NativeWebKit,
        script_evaluation_path: ScriptEvaluationPath::PrivateContentWorldPull,
        page_world_evaluate_javascript_used: false,
        call_async_javascript_used: false,
        host_clipboard_before: Some(digest.clone()),
        host_clipboard_after: Some(digest),
        host_clipboard_unchanged: true,
        receipts: mediated_receipts(1, 1),
        generation: 1,
        epoch: 1,
        evidence_class: ProofEvidenceClass::ContainedBrowser,
        physical_pass_claimed: false,
        isolation_pass_claimed: false,
        vf_pass_claimed: false,
        computer_mode_enabled: false,
        admission_available: false,
        nonclaim: CLIPBOARD_KILL_GATE_NONCLAIM.into(),
        recorded_at: chrono::Utc::now(),
    }
}

#[test]
fn linux_runner_is_unsupported_never_pass() {
    let evidence = run_clipboard_kill_gate().expect("artifact");
    assert!(!evidence.physical_pass_claimed);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert!(!evidence.computer_mode_enabled);
    assert!(!evidence.admission_available);
    assert!(!isolated_surface_admission_available());
    assert_eq!(evidence.nonclaim, CLIPBOARD_KILL_GATE_NONCLAIM);
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    verify_clipboard_kill_gate_evidence(&evidence).expect("honest linux");

    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(evidence.platform, ClipboardKillGatePlatform::NonMacOs);
        assert_eq!(
            evidence.outcome,
            ClipboardKillGateOutcome::UnsupportedPlatform
        );
        assert_eq!(evidence.verdict, ClipboardKillGateVerdict::Unsupported);
        assert!(ClipboardWitness::seal_digest().is_err());
        assert!(!WKClipboardProbe::webkit_available());
    }
}

#[test]
fn checklist_cli_and_runner_are_exclusive() {
    let request = parse_sep18_checklist_run_args(&args(&[
        "--clipboard-kill-gate",
        "-o",
        "clipboard-kill-gate.json",
    ]))
    .expect("exclusive flag");
    assert_eq!(
        request.config.substrate,
        Sep18ChecklistSubstrate::ClipboardKillGate
    );

    let err = parse_sep18_checklist_run_args(&args(&["--clipboard-kill-gate", "--vf-dry-run"]))
        .expect_err("exclusive");
    assert!(err.message.contains("--clipboard-kill-gate"));

    let err = parse_sep18_checklist_run_args(&args(&[
        "--clipboard-kill-gate",
        "--native-host-sentinels",
        "--vf-dry-run",
        "--checkout",
        "/explicit/checkout",
    ]))
    .expect_err("not with native");
    assert!(err.message.contains("--clipboard-kill-gate"));
}

#[test]
fn runner_pack_verifies_without_pass_or_admission() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::clipboard_kill_gate(),
    );
    assert!(outcome.runner_error.is_none(), "{:?}", outcome.runner_error);
    assert_eq!(
        outcome.pack.substrate,
        Sep18ChecklistSubstrate::ClipboardKillGate
    );
    assert!(!outcome.pack.physical_pass_claimed);
    assert!(!outcome.pack.isolation_pass_claimed);
    assert!(!outcome.pack.vf_pass_claimed);
    assert!(!outcome.pack.admission_available);
    assert_eq!(
        outcome.pack.physical_proof_markers,
        PhysicalProofMarkers::dry_run_none()
    );
    assert_eq!(outcome.pack.nonclaim, CLIPBOARD_KILL_GATE_NONCLAIM);
    let decision = verify_evidence_pack(&outcome.pack);
    assert!(decision.accepted, "{decision:?}");
    let gate = outcome.pack.clipboard_kill_gate.as_ref().expect("nested");
    assert_ne!(gate.verdict, ClipboardKillGateVerdict::Pass);
}

#[test]
fn forged_pass_on_linux_pack_is_rejected() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::clipboard_kill_gate(),
    );
    let mut pack = outcome.pack;
    if let Some(gate) = pack.clipboard_kill_gate.as_mut() {
        gate.verdict = ClipboardKillGateVerdict::Pass;
        gate.outcome = ClipboardKillGateOutcome::MediationProven;
    }
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGatePassOnUnsupportedPlatform
    );
}

#[test]
fn forged_macos_pass_is_rejected_on_non_macos_verifier() {
    let pack = seal_clipboard_kill_gate_pack(forged_native_pass_evidence());
    let decision = verify_evidence_pack(&pack);
    #[cfg(not(target_os = "macos"))]
    {
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::ClipboardKillGatePassOnUnsupportedPlatform
        );
    }
    #[cfg(target_os = "macos")]
    {
        let _ = decision;
    }
}

#[test]
fn synthetic_pass_fixture_is_rejected() {
    let evidence =
        ClipboardKillGateEvidence::synthetic_verifier_fixture(ClipboardKillGateVerdict::Pass, None);
    let pack = seal_clipboard_kill_gate_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateSyntheticCannotPass
    );
}

#[test]
fn host_digest_drift_rejects_pass() {
    let mut evidence = forged_native_pass_evidence();
    evidence.host_clipboard_after = Some(HostClipboardDigest::canonical(b"drifted", 3));
    evidence.host_clipboard_unchanged = true;
    let pack = seal_clipboard_kill_gate_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert!(matches!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateHostDigestDrift
            | EvidenceVerifierCode::ClipboardKillGatePassOnUnsupportedPlatform
            | EvidenceVerifierCode::ClipboardKillGateUncertainCannotPass
    ));
}

#[test]
fn stale_generation_is_rejected() {
    let pull = ProbePull::private(4, 1);
    let mut reply = ProbeReply {
        generation: 2,
        epoch: 1,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        receipts: mediated_receipts(2, 1),
    };
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("stale");
    assert_eq!(err, ClipboardProbeFailClosedReason::StaleGeneration);

    let mut evidence = forged_native_pass_evidence();
    evidence.generation = 4;
    evidence.receipts = mediated_receipts(2, 1);
    reply.generation = 2;
    let pack = seal_clipboard_kill_gate_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
}

#[test]
fn duplicate_and_missing_replies_are_rejected() {
    let pull = ProbePull::private(1, 1);
    let reply = ProbeReply {
        generation: 1,
        epoch: 1,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        receipts: mediated_receipts(1, 1),
    };
    let dup = admit_probe_reply(&pull, &reply, &[1]).expect_err("dup gen");
    assert_eq!(dup, ClipboardProbeFailClosedReason::DuplicateReply);

    let mut missing = reply.clone();
    missing.receipts.pop();
    let missing_err = admit_probe_reply(&pull, &missing, &[]).expect_err("missing");
    assert_eq!(missing_err, ClipboardProbeFailClosedReason::MissingReply);

    let mut evidence = forged_native_pass_evidence();
    evidence.receipts.pop();
    let pack = seal_clipboard_kill_gate_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert!(matches!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateMissingReply
            | EvidenceVerifierCode::ClipboardKillGatePassOnUnsupportedPlatform
            | EvidenceVerifierCode::ClipboardKillGateUncertainCannotPass
    ));
}

#[test]
fn forbidden_script_evaluation_paths_are_rejected() {
    let mut page = ProbePull::private(1, 1);
    page.world = ContentWorld::Page;
    page.path = ScriptEvaluationPath::PageWorldEvaluateJavaScript;
    let reply = ProbeReply {
        generation: 1,
        epoch: 1,
        world: ContentWorld::Page,
        path: ScriptEvaluationPath::PageWorldEvaluateJavaScript,
        receipts: mediated_receipts(1, 1),
    };
    let err = admit_probe_reply(&page, &reply, &[]).expect_err("page eval");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation
    );

    let mut async_js = reply.clone();
    async_js.path = ScriptEvaluationPath::CallAsyncJavaScript;
    let err = admit_probe_reply(&ProbePull::private(1, 1), &async_js, &[]).expect_err("async");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation
    );

    let mut evidence = forged_native_pass_evidence();
    evidence.page_world_evaluate_javascript_used = true;
    evidence.script_evaluation_path = ScriptEvaluationPath::PageWorldEvaluateJavaScript;
    let pack = seal_clipboard_kill_gate_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateForbiddenScriptEvaluation
    );

    let mut evidence = forged_native_pass_evidence();
    evidence.call_async_javascript_used = true;
    let pack = seal_clipboard_kill_gate_pack(evidence);
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateForbiddenScriptEvaluation
    );
}

#[test]
fn physical_markers_and_admission_stay_false() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::clipboard_kill_gate(),
    );
    let mut pack = outcome.pack;
    pack.physical_proof_markers = PhysicalProofMarkers {
        mac_worker_attested: true,
        live_host_sentinel_collection: true,
        physical_mac_proof_id: Some("forged-clipboard-pass".into()),
    };
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateCannotQualifyPhysicalPass
    );

    let mut pack = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::clipboard_kill_gate(),
    )
    .pack;
    pack.admission_available = true;
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(decision.code, EvidenceVerifierCode::AdmissionMustStayFalse);
}

#[test]
fn host_digest_honesty_forbids_contents_read_and_writes() {
    let before = HostClipboardDigest::canonical(b"same", 1);
    let mut after = before.clone();
    after.contents_read = true;
    assert!(!host_clipboard_unchanged(&before, &after));
    after.contents_read = false;
    after.pasteboard_mutated = true;
    assert!(!host_clipboard_unchanged(&before, &after));
}

#[test]
fn sequencer_path_matches_runner() {
    let sequencer = grokptah_isolated_surface::Sep18NoModelProofSequencer::new(
        HostSentinelSnapshot::synthetic_baseline(),
    );
    let evidence = sequencer.run_clipboard_kill_gate().expect("artifact");
    let pack = seal_clipboard_kill_gate_pack(evidence);
    assert_eq!(pack.substrate, Sep18ChecklistSubstrate::ClipboardKillGate);
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
    assert!(!isolated_surface_admission_available());
}

#[test]
fn default_cb_runner_does_not_use_clipboard_kill_gate() {
    let outcome = run_sep18_checklist(
        HostSentinelSnapshot::synthetic_baseline(),
        Sep18ChecklistRunnerConfig::contained_browser_default(),
    );
    assert_ne!(
        outcome.pack.substrate,
        Sep18ChecklistSubstrate::ClipboardKillGate
    );
    assert!(outcome.pack.clipboard_kill_gate.is_none());
}
