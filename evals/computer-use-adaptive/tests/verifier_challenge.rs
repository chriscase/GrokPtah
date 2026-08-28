use grokptah_cu_adaptive_eval::catalog::catalog;
use grokptah_cu_adaptive_eval::digest::fixture_hash;
use grokptah_cu_adaptive_eval::report::run_campaign;
use grokptah_cu_adaptive_eval::schema::{parse_strict, to_canonical_json};
use grokptah_cu_adaptive_eval::types::{
    AdapterId, CampaignStatus, Eligibility, ProcessVerdict, ProfileId,
};
use grokptah_cu_adaptive_eval::verifier::{
    process_verdict, reject_gamed_report, verify_campaign, verify_json, verify_report, VerifyMode,
};

fn clean() -> grokptah_cu_adaptive_eval::CampaignOutput {
    run_campaign(1, 435_272).unwrap()
}

#[test]
fn extra_report_fields_fail_closed() {
    let out = clean();
    let json = to_canonical_json(&out.report).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("extra".into(), serde_json::json!(true));
    assert!(parse_strict::<grokptah_cu_adaptive_eval::CampaignReport>(&value.to_string()).is_err());
}

#[test]
fn fabricated_cost_and_live_claim_fail_verifier() {
    let mut out = clean();
    out.report.episodes[0].metrics.cost_usd = Some(0.01);
    out.report.metrics.cost_usd = Some(0.01);
    let v = verify_report(&out.report);
    assert!(!v.ok, "{v:?}");

    let mut out = clean();
    out.report.episodes[0].eligibility = Eligibility::LiveAuthoritative;
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn dropped_family_is_rejected() {
    let out = clean();
    let gamed = reject_gamed_report(out.report);
    assert!(!gamed.ok);
}

#[test]
fn canonical_report_roundtrip_verifies_when_campaign_is_coherent() {
    let out = clean();
    let json = to_canonical_json(&out.report).unwrap();
    let parsed: grokptah_cu_adaptive_eval::CampaignReport = parse_strict(&json).unwrap();
    assert_eq!(parsed.source_gate.git_sha, out.report.source_gate.git_sha);
    let v = verify_json(&json).unwrap();
    if out.report.status == CampaignStatus::FailClosed {
        panic!("unexpected safety fail in synthetic campaign");
    }
    assert!(v.ok, "verifier errors on roundtrip: {:?}", v.errors);
}

#[test]
fn removed_episode_is_rejected() {
    let mut out = clean();
    out.report.episodes.pop();
    let v = verify_campaign(&out.report, Some(&out.evidence), VerifyMode::Synthetic);
    assert!(!v.ok);
    assert!(v
        .errors
        .iter()
        .any(|e| e.contains("episode count") || e.contains("missing")));
}

#[test]
fn duplicate_episode_is_rejected() {
    let mut out = clean();
    let dup = out.report.episodes[0].clone();
    out.report.episodes.insert(1, dup);
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn substituted_adapter_or_profile_is_rejected() {
    let mut out = clean();
    out.report.episodes[0].adapter = AdapterId::FrontierMultimodal;
    out.report.episodes[0].profile = ProfileId::HighAssurance;
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v
        .errors
        .iter()
        .any(|e| e.contains("schedule identity mismatch")));
}

#[test]
fn changed_expected_result_is_rejected() {
    let mut out = clean();
    out.report.episodes[0].expected_task_success = !out.report.episodes[0].expected_task_success;
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn changed_world_omitted_from_legacy_hash_is_rejected() {
    let items = catalog();
    let full = fixture_hash(&items).unwrap();
    let mut mutated = items.clone();
    mutated[0].world.run_id = "run_mutated".into();
    let after = fixture_hash(&mutated).unwrap();
    assert_ne!(full, after);
    let mut out = clean();
    out.report.fixture_hash = "0".repeat(64);
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v.errors.iter().any(|e| e.contains("fixture hash")));
}

#[test]
fn changed_aggregate_with_unchanged_episodes_is_rejected() {
    let mut out = clean();
    out.report.task_success.numerator = out.report.task_success.numerator.saturating_add(3);
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v
        .errors
        .iter()
        .any(|e| e.contains("task_success.numerator")));

    let mut out = clean();
    out.report.metrics.invalid_actions = out.report.metrics.invalid_actions.saturating_add(9);
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v.errors.iter().any(|e| e.contains("invalid_actions")));
}

#[test]
fn false_seed_is_rejected() {
    let mut out = clean();
    out.report.seed = out.report.seed.wrapping_add(99);
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn fabricated_provider_count_and_live_receipt_are_rejected() {
    let mut out = clean();
    out.report.provider_calls = 4;
    out.report.episodes[0].provider_calls = 4;
    let v = verify_report(&out.report);
    assert!(!v.ok);

    let mut out = clean();
    out.report.live_continuation.enabled = true;
    out.report.live_continuation.receipts =
        vec![grokptah_cu_adaptive_eval::live::ProviderReceipt {
            receipt_id: "forged".into(),
            provider_id: "xai".into(),
            model_id: "grok".into(),
            config_digest: "a".repeat(64),
            usage_digest: None,
            content_sha256: "b".repeat(64),
            billing: None,
        }];
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn missing_held_out_case_is_rejected() {
    let mut out = clean();
    out.report.held_out.ids.clear();
    out.report.held_out.count = 0;
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn mismatched_evidence_digest_is_rejected() {
    let mut out = clean();
    out.evidence.items[0].content_sha256 = "c".repeat(64);
    let v = verify_campaign(&out.report, Some(&out.evidence), VerifyMode::Synthetic);
    assert!(!v.ok);
    assert!(v.errors.iter().any(|e| e.contains("digest")));
}

#[test]
fn missing_evidence_reference_is_rejected() {
    let mut out = clean();
    out.report.episodes[0].evidence_ref = "ev_missing".into();
    let v = verify_campaign(&out.report, Some(&out.evidence), VerifyMode::Synthetic);
    assert!(!v.ok);
}

#[test]
fn partial_process_does_not_exit_zero() {
    let mut out = clean();
    out.report.episodes.pop();
    out.report.status = CampaignStatus::Partial;
    let v = verify_report(&out.report);
    assert_ne!(v.terminal_verdict.exit_code(), 0);
    assert_ne!(
        process_verdict(Some(&out.report), v.errors.is_empty()).exit_code(),
        0
    );
}

#[test]
fn unauthorized_hidden_by_modified_summary_is_rejected() {
    let mut out = clean();
    out.report.episodes[0].metrics.unauthorized_dispatches = 1;
    out.report.episodes[0].safety.violation = true;
    out.report.safety.unauthorized_dispatches = 0;
    out.report.safety.violations = 0;
    out.report.safety.release_failing = false;
    out.report.status = CampaignStatus::Pass;
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v
        .errors
        .iter()
        .any(|e| e.contains("unauthorized") || e.contains("hidden")));
}

#[test]
fn rust_required_episode_fields_are_required_in_schema() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../schemas/grokptah-cu-eval-result.v1.schema.json"
    ))
    .unwrap();
    let required = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    for field in [
        "expectedOutcome",
        "expectedTaskSuccess",
        "fixtureMatch",
        "episodeId",
        "evidenceRef",
    ] {
        assert!(
            required.contains(&field),
            "{field} missing from schema required"
        );
    }
    let report_schema: serde_json::Value = serde_json::from_str(include_str!(
        "../schemas/grokptah-cu-eval-report.v1.schema.json"
    ))
    .unwrap();
    assert_eq!(
        report_schema["properties"]["episodes"]["items"]["$ref"],
        "urn:grokptah:schema:cu-eval-result:v1"
    );
}

#[test]
fn live_reusable_schema_is_rejected_in_synthetic_mode() {
    let mut out = clean();
    out.report.episodes[0].eligibility = Eligibility::LiveReusableSchema;
    out.report.episodes[0].provider_calls = 0;
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v.errors.iter().any(|e| e.contains("synthetic_only")));
}

#[test]
fn rewritten_episode_seed_is_rejected() {
    let mut out = clean();
    out.report.episodes[0].seed = 1;
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(v.errors.iter().any(|e| e.contains("episode seed")));
}

#[test]
fn extra_trace_fields_fail_closed() {
    let out = clean();
    let json = to_canonical_json(&out.evidence).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let traces = value["items"][0]["trace"].as_array_mut().unwrap();
    if traces.is_empty() {
        traces.push(serde_json::json!({
            "step": 0,
            "clockMs": 0,
            "kind": "observe",
            "detail": "x",
            "secret": "needle"
        }));
    } else {
        traces[0]
            .as_object_mut()
            .unwrap()
            .insert("secret".into(), serde_json::json!("needle"));
    }
    assert!(parse_strict::<grokptah_cu_adaptive_eval::EvidenceSet>(&value.to_string()).is_err());
}

#[test]
fn claimed_release_failing_true_without_violations_is_rejected() {
    let mut out = clean();
    assert!(!out.report.safety.release_failing);
    assert_eq!(out.report.safety.violations, 0);
    assert_eq!(out.report.safety.unauthorized_dispatches, 0);
    out.report.safety.release_failing = true;
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(
        v.errors.iter().any(|e| e.contains("release_failing")),
        "{:?}",
        v.errors
    );
    assert_eq!(v.terminal_verdict, ProcessVerdict::VerifierError);
}

#[test]
fn claimed_release_failing_false_with_episode_violation_is_rejected() {
    let mut out = clean();
    out.report.episodes[0].metrics.unauthorized_dispatches = 1;
    out.report.episodes[0].safety.violation = true;
    out.report.safety.release_failing = false;
    out.report.safety.unauthorized_dispatches = 1;
    out.report.safety.violations = 1;
    out.report.status = CampaignStatus::Pass;
    let v = verify_report(&out.report);
    assert!(!v.ok);
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("release_failing") || e.contains("status")),
        "{:?}",
        v.errors
    );
}

#[test]
fn extra_episode_result_fields_fail_closed() {
    let out = clean();
    let json = to_canonical_json(&out.report.episodes[0]).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("smuggled".into(), serde_json::json!("x"));
    assert!(
        parse_strict::<grokptah_cu_adaptive_eval::runner::EpisodeResult>(&value.to_string())
            .is_err()
    );
}

#[test]
fn required_nullable_cost_round_trips_as_null() {
    let out = clean();
    assert!(out.report.metrics.cost_usd.is_none());
    let json = to_canonical_json(&out.report).unwrap();
    let parsed: grokptah_cu_adaptive_eval::CampaignReport = parse_strict(&json).unwrap();
    assert!(parsed.metrics.cost_usd.is_none());
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(value["metrics"]["costUsd"].is_null());
}

#[test]
fn evidence_body_digest_is_result_and_matches_stored_hash() {
    let out = clean();
    let item = &out.evidence.items[0];
    let digest = grokptah_cu_adaptive_eval::digest::evidence_body_digest(item).unwrap();
    assert_eq!(digest, item.content_sha256);
}

#[test]
fn pass_verdict_is_the_only_zero_exit() {
    assert_eq!(ProcessVerdict::Pass.exit_code(), 0);
    for v in [
        ProcessVerdict::Partial,
        ProcessVerdict::FailClosed,
        ProcessVerdict::VerifierError,
        ProcessVerdict::Malformed,
        ProcessVerdict::InvalidRepeats,
        ProcessVerdict::LiveRefused,
    ] {
        assert_ne!(v.exit_code(), 0, "{v:?}");
    }
}
