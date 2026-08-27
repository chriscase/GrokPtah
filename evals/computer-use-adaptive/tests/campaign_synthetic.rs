use grokptah_cu_adaptive_eval::catalog::{catalog, validate_catalog};
use grokptah_cu_adaptive_eval::report::run_campaign;
use grokptah_cu_adaptive_eval::types::{CampaignStatus, FamilyId, ProfileId};
use grokptah_cu_adaptive_eval::verifier::verify_report;
use grokptah_cu_adaptive_eval::SOURCE_GATE_SHA;

#[test]
fn synthetic_campaign_zero_provider_calls_and_zero_unauthorized() {
    let items = catalog();
    validate_catalog(&items).unwrap();
    let out = run_campaign(2, 435_272);
    assert_eq!(out.report.source_gate.git_sha, SOURCE_GATE_SHA);
    assert_eq!(out.report.provider_calls, 0);
    assert_eq!(out.report.safety.unauthorized_dispatches, 0);
    assert_eq!(out.report.safety.violations, 0);
    assert!(!out.report.anti_gaming.fabricated_cost);
    assert!(!out.report.anti_gaming.live_claim_from_fake);
    assert_eq!(out.report.families.len(), FamilyId::ALL.len());
    for family in FamilyId::ALL {
        assert!(out
            .report
            .episodes
            .iter()
            .any(|e| e.family == family.as_str()));
    }
    for profile in ProfileId::ALL {
        assert!(out.report.episodes.iter().any(|e| e.profile == profile));
    }
    assert!(out.report.held_out.count >= 1);
    assert!(out.report.metrics.cost_usd.is_none());
    let verified = verify_report(&out.report);
    assert!(verified.ok, "verifier errors: {:?}", verified.errors);
    assert_eq!(out.report.repeats, 2);
    assert_eq!(
        out.report.status,
        CampaignStatus::Pass,
        "mismatched cells: {:?}",
        out.report
            .episodes
            .iter()
            .filter(|e| !e.fixture_match)
            .map(|e| format!(
                "{} {} {} got={:?} expected={:?}",
                e.scenario_id,
                e.profile.as_str(),
                e.adapter.as_str(),
                e.outcome_class,
                e.expected_outcome
            ))
            .take(30)
            .collect::<Vec<_>>()
    );
    let stale_family: Vec<_> = out
        .report
        .episodes
        .iter()
        .filter(|e| {
            e.variant == "stale_observation" && e.adapter.as_str() != "malformed_overconfident"
        })
        .collect();
    assert!(stale_family
        .iter()
        .any(|e| e.metrics.stale_action_attempts > 0));
}

#[test]
fn bounded_repeats_are_deterministic() {
    let a = run_campaign(2, 435_272);
    let b = run_campaign(2, 435_272);
    assert_eq!(a.report.fixture_hash, b.report.fixture_hash);
    assert_eq!(a.report.episodes.len(), b.report.episodes.len());
    for (l, r) in a.report.episodes.iter().zip(b.report.episodes.iter()) {
        assert_eq!(l.outcome_class, r.outcome_class);
        assert_eq!(l.task_success, r.task_success);
        assert_eq!(
            l.metrics.unauthorized_dispatches,
            r.metrics.unauthorized_dispatches
        );
        assert_eq!(l.metrics.physical_dispatches, r.metrics.physical_dispatches);
        assert_eq!(l.metrics.observation_bytes, r.metrics.observation_bytes);
    }
}

#[test]
fn economy_uses_no_images_on_semantic_suite() {
    let out = run_campaign(1, 435_272);
    let economy_semantic: Vec<_> = out
        .report
        .episodes
        .iter()
        .filter(|e| e.profile == ProfileId::Economy && e.family == "unique_semantic_no_screenshot")
        .collect();
    assert!(!economy_semantic.is_empty());
    assert!(economy_semantic.iter().all(|e| e.metrics.image_bytes == 0));
}
