use grokptah_cu_adaptive_eval::report::run_campaign;
use grokptah_cu_adaptive_eval::schema::{parse_strict, to_canonical_json};
use grokptah_cu_adaptive_eval::types::{CampaignStatus, Eligibility};
use grokptah_cu_adaptive_eval::verifier::{reject_gamed_report, verify_json, verify_report};

#[test]
fn extra_report_fields_fail_closed() {
    let mut json =
        String::from("{\"schemaVersion\":\"grokptah.cu_eval_campaign_report.v1\",\"extra\":true}");
    json.insert_str(0, "");
    assert!(parse_strict::<grokptah_cu_adaptive_eval::CampaignReport>(&json).is_err());
}

#[test]
fn fabricated_cost_and_live_claim_fail_verifier() {
    let mut out = run_campaign(1, 435_272);
    out.report.episodes[0].metrics.cost_usd = Some(0.01);
    out.report.anti_gaming.fabricated_cost = true;
    out.report.metrics.cost_usd = Some(0.01);
    let v = verify_report(&out.report);
    assert!(!v.ok, "{v:?}");

    let mut out = run_campaign(1, 435_272);
    out.report.episodes[0].eligibility = Eligibility::LiveAuthoritative;
    out.report.anti_gaming.live_claim_from_fake = true;
    let v = verify_report(&out.report);
    assert!(!v.ok);
}

#[test]
fn dropped_family_is_rejected() {
    let out = run_campaign(1, 435_272);
    let gamed = reject_gamed_report(out.report);
    assert!(!gamed.ok);
}

#[test]
fn canonical_report_roundtrip_verifies_when_campaign_is_coherent() {
    let out = run_campaign(1, 435_272);
    let json = to_canonical_json(&out.report).unwrap();
    let parsed: grokptah_cu_adaptive_eval::CampaignReport = parse_strict(&json).unwrap();
    assert_eq!(parsed.source_gate.git_sha, out.report.source_gate.git_sha);
    let v = verify_json(&json).unwrap();
    if out.report.status == CampaignStatus::FailClosed {
        panic!("unexpected safety fail in synthetic campaign");
    }
    assert!(v.ok, "verifier errors on roundtrip: {:?}", v.errors);
}
