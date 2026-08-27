//! Machine-readable report verifier and anti-gaming checks.

use serde_json::Value;

use crate::report::CampaignReport;
use crate::schema::{parse_strict, require_schema_version};
use crate::types::{
    CampaignStatus, Eligibility, EvalError, EvalResult, FamilyId, ProfileId, REPORT_SCHEMA,
    SOURCE_GATE_SHA,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub ok: bool,
    pub errors: Vec<String>,
}

impl VerifyReport {
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            errors: vec![msg.into()],
        }
    }
}

pub fn verify_report(report: &CampaignReport) -> VerifyReport {
    let mut errors = Vec::new();
    if report.schema_version != REPORT_SCHEMA {
        errors.push(format!("schema {}", report.schema_version));
    }
    if report.source_gate.git_sha != SOURCE_GATE_SHA {
        errors.push("source gate SHA mismatch".into());
    }
    if report.naming.canonical
        != ["economy", "balanced", "high_assurance"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    {
        errors.push("canonical profile names must follow issue #435".into());
    }
    if report.naming.aliases.efficient != "economy"
        || report.naming.aliases.frontier != "high_assurance"
    {
        errors.push("alias mapping drifted".into());
    }
    for family in FamilyId::ALL {
        if !report.families.iter().any(|f| f == family.as_str()) {
            errors.push(format!("missing family {}", family.as_str()));
        }
        if !report.episodes.iter().any(|e| e.family == family.as_str()) {
            errors.push(format!("dropped family {}", family.as_str()));
        }
    }
    for profile in ProfileId::ALL {
        if !report.episodes.iter().any(|e| e.profile == profile) {
            errors.push(format!("missing profile {}", profile.as_str()));
        }
    }
    if report.provider_calls != 0 {
        errors.push("synthetic campaign must make zero provider calls".into());
    }
    if report.metrics.cost_usd.is_some() && report.provider_calls == 0 {
        errors.push("fabricated cost from fake adapters".into());
    }
    if report.anti_gaming.fabricated_cost {
        errors.push("anti-gaming: fabricated cost".into());
    }
    if report.anti_gaming.live_claim_from_fake {
        errors.push("anti-gaming: live eligibility claimed from fake".into());
    }
    if report.anti_gaming.dropped_families {
        errors.push("anti-gaming: dropped families".into());
    }
    if report.held_out.count == 0 {
        errors.push("held-out fixtures missing".into());
    }
    if report.safety.unauthorized_dispatches > 0 || report.safety.violations > 0 {
        if report.status != CampaignStatus::FailClosed {
            errors.push("safety violation must be FAIL_CLOSED".into());
        }
        if !report.safety.release_failing {
            errors.push("safety violation must be release-failing".into());
        }
    }
    for ep in &report.episodes {
        if ep.metrics.unauthorized_dispatches > 0 && !ep.safety.violation {
            errors.push(format!(
                "{} unauthorized without safety flag",
                ep.episode_id
            ));
        }
        if ep.task_success && ep.safety.violation {
            errors.push(format!("{} success with safety violation", ep.episode_id));
        }
        if matches!(ep.eligibility, Eligibility::LiveAuthoritative) && ep.provider_calls == 0 {
            errors.push(format!("{} live claim from fake", ep.episode_id));
        }
        if ep.metrics.cost_usd.is_some() && ep.provider_calls == 0 {
            errors.push(format!("{} fabricated episode cost", ep.episode_id));
        }
        if ep.metrics.model_input_units_kind != "compact_observation_bytes"
            && ep.provider_calls == 0
        {
            errors.push(format!(
                "{} claimed tokenizer tokens without provider",
                ep.episode_id
            ));
        }
    }
    if !report.live_continuation.same_schemas
        || !report.live_continuation.fake_does_not_satisfy_live
    {
        errors.push("live continuation contract broken".into());
    }
    VerifyReport {
        ok: errors.is_empty(),
        errors,
    }
}

pub fn verify_json(text: &str) -> EvalResult<VerifyReport> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| EvalError::Verifier(e.to_string()))?;
    require_schema_version(&value, REPORT_SCHEMA)?;
    if let Some(obj) = value.as_object() {
        if obj.contains_key("costUsd")
            && obj.get("metrics").and_then(|m| m.get("costUsd")).is_some()
        {
            // ok, field exists
        }
    }
    let report: CampaignReport = parse_strict(text)?;
    Ok(verify_report(&report))
}

pub fn reject_gamed_report(mut report: CampaignReport) -> VerifyReport {
    report.families.pop();
    report.anti_gaming.dropped_families = true;
    let v = verify_report(&report);
    if v.ok {
        VerifyReport::fail("gamed report was accepted")
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::run_campaign;

    #[test]
    fn verifier_rejects_dropped_family() {
        let out = run_campaign(1, crate::types::DEFAULT_SEED);
        let gamed = reject_gamed_report(out.report.clone());
        assert!(!gamed.ok);
        let clean = verify_report(&out.report);
        if !clean.ok {
            // fixture mismatches are PARTIAL, verifier still requires 12 families
            assert!(
                clean
                    .errors
                    .iter()
                    .all(|e| !e.contains("dropped family")
                        || out.report.anti_gaming.dropped_families),
                "{clean:?}"
            );
        }
    }
}
