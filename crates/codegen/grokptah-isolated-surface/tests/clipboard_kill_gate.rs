//! Contained Browser clipboard isolation kill-gate — adversarial verifier +
//! Linux fail-closed harness.
//!
//! Synthetic fixtures prove the machine verifier. They cannot establish
//! physical Mac Pass. Computer Mode and admission stay false.

use grokptah_isolated_surface::{
    admit_private_world_handler_messages, admit_probe_reply, clipboard_kill_gate_may_claim_pass,
    decode_page_result_bytes, host_clipboard_unchanged, isolated_surface_admission_available,
    native_clipboard_probe_authorized, page_world_initiator_source, parse_evidence_pack,
    parse_sep18_checklist_run_args, probe_reply_from_page_result, receipts_all_fulfilled,
    run_sep18_checklist, seal_clipboard_kill_gate_pack, serialize_evidence_pack,
    verify_clipboard_kill_gate_evidence, verify_evidence_pack, ClipboardKillGateEvidence,
    ClipboardKillGateOutcome, ClipboardKillGatePlatform, ClipboardKillGateProvenance,
    ClipboardKillGateVerdict, ClipboardOperation, ClipboardProbeFailClosedReason, ContentWorld,
    EvidenceVerifierCode, HostClipboardDigest, HostSentinelSnapshot, PageLocalClipboardReceipt,
    PhysicalProofMarkers, ProbePull, ProbeReply, ProofEvidenceClass, ReceiptInitiator,
    ReplyChannel, ScriptEvaluationPath, Sep18ChecklistRunnerConfig, Sep18ChecklistSubstrate,
    CF_RUN_LOOP_DEFAULT_MODE, CLIPBOARD_KILL_GATE_NONCLAIM, MAX_PROBE_REPLY_BYTES,
    PAGE_RESULT_ATTRIBUTE, PAGE_WORLD_INTERCEPTOR_SOURCE, PRIVATE_REPLY_TITLE_PREFIX,
    PRIVATE_WORLD_PULL_SOURCE,
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
            initiator: ReceiptInitiator::PageWorld,
            attempted: true,
            settled: true,
            fulfilled: true,
            unavailable: false,
            api: operation.api_name(),
        })
        .collect()
}

fn page_result_json(generation: u64, epoch: u64) -> String {
    let receipts: Vec<_> = ClipboardOperation::ALL
        .into_iter()
        .map(|operation| {
            serde_json::json!({
                "operation": operation,
                "generation": generation,
                "epoch": epoch,
                "initiator": "page_world",
                "attempted": true,
                "settled": true,
                "fulfilled": true,
                "unavailable": false,
                "api": operation.api_name(),
            })
        })
        .collect();
    serde_json::json!({
        "generation": generation,
        "epoch": epoch,
        "initiator": "page_world",
        "receipts": receipts,
    })
    .to_string()
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
        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
        page_world_participated: true,
        private_world_initiated_operations: false,
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
        live_authority: None,
    }
}

#[test]
fn linux_runner_is_unsupported_never_pass() {
    // This assertion is about the non-macOS evidence contract. Keep it a
    // deterministic fixture even when the test suite itself runs on macOS;
    // native WebKit execution belongs to the explicit physical runner.
    let evidence = ClipboardKillGateEvidence::unsupported_non_macos();
    assert!(!evidence.physical_pass_claimed);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert!(!evidence.computer_mode_enabled);
    assert!(!evidence.admission_available);
    assert!(!isolated_surface_admission_available());
    assert_eq!(evidence.nonclaim, CLIPBOARD_KILL_GATE_NONCLAIM);
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    verify_clipboard_kill_gate_evidence(&evidence).expect("honest linux");

    assert_eq!(evidence.platform, ClipboardKillGatePlatform::NonMacOs);
    assert_eq!(
        evidence.outcome,
        ClipboardKillGateOutcome::UnsupportedPlatform
    );
    assert_eq!(evidence.verdict, ClipboardKillGateVerdict::Unsupported);
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
    let pack = seal_clipboard_kill_gate_pack(ClipboardKillGateEvidence::unsupported_non_macos());
    assert_eq!(pack.substrate, Sep18ChecklistSubstrate::ClipboardKillGate);
    assert!(!pack.physical_pass_claimed);
    assert!(!pack.isolation_pass_claimed);
    assert!(!pack.vf_pass_claimed);
    assert!(!pack.admission_available);
    assert_eq!(
        pack.physical_proof_markers,
        PhysicalProofMarkers::dry_run_none()
    );
    assert_eq!(pack.nonclaim, CLIPBOARD_KILL_GATE_NONCLAIM);
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
    let gate = pack.clipboard_kill_gate.as_ref().expect("nested");
    assert_ne!(gate.verdict, ClipboardKillGateVerdict::Pass);
}

#[test]
fn forged_pass_on_linux_pack_is_rejected() {
    // This is a verifier mutation test, not a native probe. Construct the
    // unsupported artifact directly so macOS test runs never initialize
    // WebKit merely to exercise a forged non-macOS claim.
    let mut pack =
        seal_clipboard_kill_gate_pack(ClipboardKillGateEvidence::unsupported_non_macos());
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
fn forged_macos_pass_is_rejected_without_live_witness() {
    let pack = seal_clipboard_kill_gate_pack(forged_native_pass_evidence());
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert!(
        decision.code == EvidenceVerifierCode::ClipboardKillGateLiveWitnessMissing
            || decision.code == EvidenceVerifierCode::ClipboardKillGatePassOnUnsupportedPlatform
            || decision.code == EvidenceVerifierCode::ClipboardKillGateUncertainCannotPass,
        "{decision:?}"
    );
    assert!(!clipboard_kill_gate_may_claim_pass(
        pack.clipboard_kill_gate.as_ref().expect("nested")
    ));

    let json = serialize_evidence_pack(&pack).expect("json");
    let parsed = parse_evidence_pack(&json).expect("parse");
    let parsed_gate = parsed.clipboard_kill_gate.as_ref().expect("nested");
    assert!(parsed_gate.live_authority.is_none());
    let roundtrip = verify_evidence_pack(&parsed);
    assert!(!roundtrip.accepted);
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
            | EvidenceVerifierCode::ClipboardKillGateLiveWitnessMissing
    ));
}

#[test]
fn stale_generation_is_rejected() {
    let pull = ProbePull::private(4, 1);
    let reply = ProbeReply {
        generation: 2,
        epoch: 1,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
        page_world_participated: true,
        private_world_initiated_operations: false,
        receipts: mediated_receipts(2, 1),
    };
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("stale");
    assert_eq!(err, ClipboardProbeFailClosedReason::StaleGeneration);

    let mut evidence = forged_native_pass_evidence();
    evidence.generation = 4;
    evidence.receipts = mediated_receipts(2, 1);
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
        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
        page_world_participated: true,
        private_world_initiated_operations: false,
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
            | EvidenceVerifierCode::ClipboardKillGateLiveWitnessMissing
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
        reply_channel: ReplyChannel::EvaluateJavaScript,
        page_world_participated: true,
        private_world_initiated_operations: false,
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
    let mut pack =
        seal_clipboard_kill_gate_pack(ClipboardKillGateEvidence::unsupported_non_macos());
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

    let mut pack =
        seal_clipboard_kill_gate_pack(ClipboardKillGateEvidence::unsupported_non_macos());
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
#[cfg(not(target_os = "macos"))]
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

#[test]
fn private_world_generated_receipts_cannot_seal_pass() {
    let pull = ProbePull::private(1, 1);
    let mut reply = ProbeReply {
        generation: 1,
        epoch: 1,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
        page_world_participated: true,
        private_world_initiated_operations: true,
        receipts: mediated_receipts(1, 1),
    };
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("private ops");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
    );

    reply.private_world_initiated_operations = false;
    reply.receipts[1].initiator = ReceiptInitiator::PrivateWorld;
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("private receipt");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
    );

    let mut evidence = forged_native_pass_evidence();
    evidence.private_world_initiated_operations = true;
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("evidence");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
    );
    let decision = verify_evidence_pack(&seal_clipboard_kill_gate_pack(evidence));
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGatePrivateWorldGeneratedReceipts
    );
}

#[test]
fn page_world_nonparticipation_cannot_seal_pass() {
    let err = decode_page_result_bytes(b"").expect_err("empty mailbox");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::PageWorldNonparticipation
    );

    let mut evidence = forged_native_pass_evidence();
    evidence.page_world_participated = false;
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("evidence");
    assert_eq!(
        err,
        ClipboardProbeFailClosedReason::PageWorldNonparticipation
    );
    let decision = verify_evidence_pack(&seal_clipboard_kill_gate_pack(evidence));
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGatePageWorldNonparticipation
    );
}

#[test]
fn title_channel_replies_cannot_seal_pass() {
    let mut evidence = forged_native_pass_evidence();
    evidence.reply_channel = ReplyChannel::DocumentTitle;
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("title");
    assert_eq!(err, ClipboardProbeFailClosedReason::UntrustedReplyChannel);
    let decision = verify_evidence_pack(&seal_clipboard_kill_gate_pack(evidence));
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateUntrustedReplyChannel
    );
}

#[test]
fn dom_attribute_tampering_cannot_seal_pass() {
    let interceptor_attr =
        r#"[{"operation":"copy","mediatedPageLocal":true,"hostPasteboardTouched":false}]"#;
    let err = decode_page_result_bytes(interceptor_attr.as_bytes()).expect_err("tamper list");
    assert_ne!(err, ClipboardProbeFailClosedReason::Timeout);
    assert!(
        err == ClipboardProbeFailClosedReason::MalformedReply
            || err == ClipboardProbeFailClosedReason::UnknownWireField
    );

    let mut tampered: serde_json::Value =
        serde_json::from_str(&page_result_json(1, 1)).expect("page json");
    tampered["forged"] = serde_json::json!(true);
    let err = decode_page_result_bytes(tampered.to_string().as_bytes()).expect_err("extra");
    assert_eq!(err, ClipboardProbeFailClosedReason::UnknownWireField);

    let mut evidence = forged_native_pass_evidence();
    evidence.reply_channel = ReplyChannel::DomAttribute;
    let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("dom channel");
    assert_eq!(err, ClipboardProbeFailClosedReason::UntrustedReplyChannel);
    let decision = verify_evidence_pack(&seal_clipboard_kill_gate_pack(evidence));
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::ClipboardKillGateUntrustedReplyChannel
    );
}

#[test]
fn extra_unknown_fields_and_oversized_reply_cannot_pass() {
    let mut extra: serde_json::Value =
        serde_json::from_str(&page_result_json(1, 1)).expect("page json");
    extra["path"] = serde_json::json!("private_content_world_pull");
    let err = decode_page_result_bytes(extra.to_string().as_bytes()).expect_err("unknown");
    assert_eq!(err, ClipboardProbeFailClosedReason::UnknownWireField);

    let oversized = vec![b'{'; MAX_PROBE_REPLY_BYTES + 1];
    let err = decode_page_result_bytes(&oversized).expect_err("oversized");
    assert_eq!(err, ClipboardProbeFailClosedReason::OversizedReply);
}

#[test]
fn stale_challenge_and_duplicate_handler_messages_cannot_pass() {
    let pull = ProbePull::private(9, 2);
    let page = decode_page_result_bytes(page_result_json(1, 2).as_bytes()).expect("decode");
    let reply = probe_reply_from_page_result(page, ReplyChannel::PrivateWorldScriptMessageHandler);
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("stale");
    assert_eq!(err, ClipboardProbeFailClosedReason::StaleGeneration);

    let body = page_result_json(1, 1).into_bytes();
    let err =
        admit_private_world_handler_messages(&ProbePull::private(1, 1), &[body.clone(), body], &[])
            .expect_err("duplicate");
    assert_eq!(err, ClipboardProbeFailClosedReason::DuplicateReply);
}

#[test]
fn private_world_script_is_read_only_and_page_world_initiates_async_clipboard() {
    assert!(!PRIVATE_WORLD_PULL_SOURCE.contains("execCommand"));
    assert!(!PRIVATE_WORLD_PULL_SOURCE.contains("writeText"));
    assert!(!PRIVATE_WORLD_PULL_SOURCE.contains("readText"));
    assert!(!PRIVATE_WORLD_PULL_SOURCE.contains("document.title"));
    assert!(!PRIVATE_WORLD_PULL_SOURCE.contains(PRIVATE_REPLY_TITLE_PREFIX));
    assert!(!PRIVATE_WORLD_PULL_SOURCE.contains("mediatedPageLocal"));
    assert!(PRIVATE_WORLD_PULL_SOURCE.contains("postMessage"));
    assert!(PRIVATE_WORLD_PULL_SOURCE.contains(PAGE_RESULT_ATTRIBUTE));

    assert!(!PAGE_WORLD_INTERCEPTOR_SOURCE.contains("execCommand"));
    assert!(!PAGE_WORLD_INTERCEPTOR_SOURCE.contains(PAGE_RESULT_ATTRIBUTE));
    assert!(!PAGE_WORLD_INTERCEPTOR_SOURCE.contains("writeText ="));
    assert!(!PAGE_WORLD_INTERCEPTOR_SOURCE.contains("readText ="));

    let initiator = page_world_initiator_source(11, 4);
    assert!(!initiator.contains("execCommand"));
    assert!(!initiator.contains("mediatedPageLocal"));
    assert!(initiator.contains("clip.readText"));
    assert!(initiator.contains("clip.writeText"));
    assert!(initiator.contains("invoke(\"read\", \"read\""));
    assert!(initiator.contains("invoke(\"write\", \"write\""));
    assert!(initiator.contains("var generation = 11;"));
    assert!(initiator.contains("initiator: \"page_world\""));
    assert!(!initiator.contains("document.title"));
    assert!(!initiator.contains("postMessage"));
    assert!(!initiator.contains("writeText = function"));
}

#[test]
fn runloop_pump_uses_cf_default_mode_not_ns_symbol_name() {
    assert_eq!(CF_RUN_LOOP_DEFAULT_MODE, "kCFRunLoopDefaultMode");
    assert_ne!(CF_RUN_LOOP_DEFAULT_MODE, "NSDefaultRunLoopMode");
    let source = include_str!("../src/wk_clipboard_probe.rs");
    assert!(source.contains("static kCFRunLoopDefaultMode"));
    assert!(source.contains("CoreFoundation"));
    assert!(!source.contains("nsstring(\"NSDefaultRunLoopMode\")"));
    assert!(!source.contains("\"NSDefaultRunLoopMode\""));
}

#[test]
fn cargo_test_never_authorizes_native_webkit() {
    assert!(!native_clipboard_probe_authorized());
}

#[test]
fn stubbed_hardcoded_and_incomplete_async_clipboard_receipts_cannot_pass() {
    let hardcoded = serde_json::json!({
        "generation": 1,
        "epoch": 1,
        "initiator": "page_world",
        "receipts": [{
            "operation": "copy",
            "generation": 1,
            "epoch": 1,
            "mediatedPageLocal": true,
            "hostPasteboardTouched": false,
            "initiator": "page_world"
        }]
    })
    .to_string();
    let err = decode_page_result_bytes(hardcoded.as_bytes()).expect_err("hardcoded copy");
    assert!(
        err == ClipboardProbeFailClosedReason::UnknownWireField
            || err == ClipboardProbeFailClosedReason::MalformedReply
    );

    let pull = ProbePull::private(1, 1);
    let mut reply = ProbeReply {
        generation: 1,
        epoch: 1,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
        page_world_participated: true,
        private_world_initiated_operations: false,
        receipts: mediated_receipts(1, 1),
    };
    reply.receipts[0].attempted = false;
    reply.receipts[0].unavailable = true;
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("unavailable");
    assert_eq!(err, ClipboardProbeFailClosedReason::ClipboardApiUnavailable);

    let mut missing_read = mediated_receipts(1, 1);
    missing_read.retain(|receipt| receipt.operation != ClipboardOperation::ReadText);
    let mut reply = ProbeReply {
        generation: 1,
        epoch: 1,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
        page_world_participated: true,
        private_world_initiated_operations: false,
        receipts: missing_read,
    };
    let err = admit_probe_reply(&pull, &reply, &[]).expect_err("missing readText");
    assert_eq!(err, ClipboardProbeFailClosedReason::MissingReply);

    reply.receipts = mediated_receipts(1, 1);
    reply.receipts[3].fulfilled = false;
    let admitted = admit_probe_reply(&pull, &reply, &[]).expect("settled but unfulfilled");
    assert!(!receipts_all_fulfilled(&admitted.receipts));

    let mut evidence = forged_native_pass_evidence();
    evidence.receipts[3].fulfilled = false;
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("unfulfilled");
    assert_eq!(err, ClipboardProbeFailClosedReason::ClipboardApiUnavailable);
}

#[test]
fn mutation_controls_never_enable_admission_or_computer_mode() {
    let mut evidence = forged_native_pass_evidence();
    evidence.reply_channel = ReplyChannel::DocumentTitle;
    evidence.computer_mode_enabled = true;
    evidence.admission_available = true;
    assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    assert!(!isolated_surface_admission_available());
    let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("closed");
    assert_eq!(err, ClipboardProbeFailClosedReason::Uncertain);
}
