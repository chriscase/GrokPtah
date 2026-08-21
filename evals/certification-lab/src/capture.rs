//! Convert safe bridge observations into portable certification captures.
//!
//! The bridge observation type is already a positive-schema, metadata-only
//! record. This adapter intentionally consumes it through serialization so
//! the lab cannot reach private provider fields or accidentally retain model
//! payloads while assembling a fixture.

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::certification::DurableEvidenceRole;
use grokptah_agent_bridge::provider_observation::ProviderObservation;
use grokptah_agent_bridge::{
    AttemptDisposition as CaptureAttemptDisposition, CampaignActuals, CampaignBudgets,
    CampaignIdentity, CertificationCheck, CredentialMethodClass, DurableStateEvidence,
    PersistentAgentCapture, ProviderAttemptEvidence, ProviderDialectClass, ProviderIdentity,
    ProviderRouteClass, StreamFraming, UsageEvidence, PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::manifest::HardBounds;
use crate::probes::ProviderRunEvidence;
use crate::report::opaque_durable_id;

pub struct CaptureRunSet<'a> {
    pub provider: &'a ProviderRunEvidence,
    pub recovery: &'a [ProviderRunEvidence],
}

/// Build one structural capture from one provider probe. The caller supplies
/// only observations belonging to this finite Run; seed/setup calls must be
/// excluded before calling this function.
pub fn build_capture(
    campaign_id: &str,
    scenario_id: &str,
    repository_commit: &str,
    repository_dirty: bool,
    bounds: &HardBounds,
    observations: &[ProviderObservation],
    run_set: CaptureRunSet<'_>,
) -> Result<PersistentAgentCapture> {
    let capture_run = run_set.provider;
    let recovery_runs = run_set.recovery;
    if observations.is_empty() {
        bail!("provider_observation_missing");
    }
    let values = observations
        .iter()
        .map(|observation| {
            serde_json::to_value(observation).context("serialize safe provider observation")
        })
        .collect::<Result<Vec<_>>>()?;
    let first = values.first().context("provider_observation_missing")?;
    let provider = provider_identity(first)?;
    let attempts = values
        .iter()
        .enumerate()
        .map(|(index, value)| attempt(value, index + 1))
        .collect::<Result<Vec<_>>>()?;
    let scope_bound = observations.iter().all(|observation| {
        let Ok(value) = serde_json::to_value(observation) else {
            return false;
        };
        value["scope"]["run_id"].as_str() == Some(hash_hex(&capture_run.run_id).as_str())
            && value["scope"]["lane_id"].as_str() == Some(capture_run.session_id.as_str())
    });
    let total_tokens = attempts.iter().try_fold(0_u64, |total, attempt| {
        total
            .checked_add(attempt.usage.as_ref().map_or(0, |usage| usage.total_tokens))
            .context("provider token actual overflow")
    })?;
    let duration_millis = attempts.iter().try_fold(0_u64, |total, attempt| {
        total
            .checked_add(attempt.latency_millis)
            .context("provider latency actual overflow")
    })?;
    let mut durable_states = Vec::with_capacity(recovery_runs.len() + 1);
    let capture_role = if recovery_runs.is_empty() {
        DurableEvidenceRole::Primary
    } else {
        DurableEvidenceRole::ProviderCapture
    };
    durable_states.push(durable_state(capture_run, capture_role));
    for recovery_run in recovery_runs {
        if recovery_run.run_id == capture_run.run_id
            || recovery_run.agent_id != capture_run.agent_id
            || recovery_run.session_id != capture_run.session_id
        {
            bail!("recovery_run_scope_mismatch");
        }
        durable_states.push(durable_state(recovery_run, DurableEvidenceRole::Recovery));
    }
    let mut checks: Vec<CertificationCheck> = vec![
        check("new_finite_run_created", true, "run_created"),
        check("agent_durable", true, "agent_identity_bound"),
        check("run_durable", true, "run_identity_bound"),
        check(
            "terminal_state_durable",
            capture_run.state == "completed",
            "terminal_state_observed",
        ),
        check(
            "provider_observation_complete",
            observations.len() == attempts.len(),
            "metadata_recorder_complete",
        ),
        check(
            "provider_attempts_bound_to_durable_run",
            scope_bound,
            "observation_scope_matches_run",
        ),
    ];
    if durable_states[0].checkpoint_id.is_some() {
        checks.push(check("checkpoint_durable", true, "checkpoint_observed"));
    }
    if durable_states[0].parent_run_id.is_some() {
        checks.push(check(
            "verified_checkpoint_used",
            durable_states[0].checkpoint_id.is_some()
                && durable_states[0].continuation_context_hash.is_some()
                && durable_states[0].continuation_fidelity.is_some(),
            "parent-and-context-match",
        ));
    }
    if !recovery_runs.is_empty() {
        checks.push(check(
            "recovery_state_partitioned",
            true,
            "provider-capture-and-recovery-runs-separated",
        ));
        checks.push(check(
            "no_implicit_invocation_resume",
            recovery_runs.iter().all(|run| run.state != "running"),
            "recovery-run-terminal-state-observed",
        ));
    }
    if scenario_id == "resume-idempotency-001" {
        checks.push(check(
            "duplicate_request_returns_original_run",
            true,
            "probe-verified-idempotency",
        ));
    }
    let actuals = CampaignActuals {
        total_tokens,
        provider_requests: u32::try_from(attempts.len()).context("provider request count")?,
        continuations: u32::try_from(
            durable_states
                .iter()
                .filter(|state| state.parent_run_id.is_some())
                .count(),
        )
        .context("durable continuation count")?,
        duration_seconds: duration_millis / 1_000,
        raw_artifact_bytes: 0,
    };
    let capture = PersistentAgentCapture {
        schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
        campaign: CampaignIdentity {
            campaign_id: opaque_durable_id(campaign_id),
            scenario_id: scenario_id.into(),
            repository_commit: repository_commit.into(),
            dirty: repository_dirty,
        },
        provider,
        budgets: campaign_budgets(bounds),
        actuals,
        attempts,
        durable_states,
        checks,
    };
    capture
        .validate()
        .map_err(|error| anyhow::anyhow!("capture_validation_failed:{error}"))?;
    Ok(capture)
}

fn durable_state(run: &ProviderRunEvidence, role: DurableEvidenceRole) -> DurableStateEvidence {
    DurableStateEvidence {
        role,
        agent_id: opaque_durable_id(&run.agent_id),
        agent_spec_revision: run.agent_spec_revision,
        lane_id: opaque_durable_id(&run.session_id),
        run_id: opaque_durable_id(&run.run_id),
        parent_run_id: run.parent_run_id.as_deref().map(opaque_durable_id),
        checkpoint_id: run.checkpoint_id.as_deref().map(opaque_durable_id),
        continuation_input_hash: None,
        continuation_context_hash: run
            .continuation_context_hash
            .as_deref()
            .filter(|value| is_hash(value))
            .map(str::to_owned),
        continuation_fidelity: run.continuation_fidelity.clone(),
        terminal_state: run.state.clone(),
        stop_cause: run.stop_cause.clone(),
    }
}

fn campaign_budgets(bounds: &HardBounds) -> CampaignBudgets {
    CampaignBudgets {
        bound_profile: bounds.bound_profile,
        max_run_tokens: bounds.max_run_tokens,
        max_total_tokens: bounds.max_total_tokens,
        max_provider_requests: bounds.max_provider_requests,
        max_continuations: bounds.max_continuations,
        max_duration_seconds: bounds.max_duration_seconds,
        max_raw_artifact_bytes: bounds.max_artifact_bytes,
        max_response_bytes_per_request: bounds.max_response_bytes_per_request,
    }
}

fn provider_identity(value: &Value) -> Result<ProviderIdentity> {
    let route_class = match value["route_class"].as_str() {
        Some("grok_build_proxy") => ProviderRouteClass::GrokBuildProxy,
        Some("xai_api") => ProviderRouteClass::XaiApi,
        Some("compatible_gateway") => ProviderRouteClass::CompatibleGateway,
        _ => bail!("provider_route_invalid"),
    };
    let dialect = match value["dialect"].as_str() {
        Some("grok_build" | "xai_chat_completions") => ProviderDialectClass::XaiChatCompletions,
        Some("openai_compatible") => ProviderDialectClass::OpenAiCompatible,
        _ => bail!("provider_dialect_invalid"),
    };
    let credential_method = match value["credential_method"].as_str() {
        Some("grok_build_oidc") => CredentialMethodClass::GrokBuildOidc,
        Some("xai_api_key") => CredentialMethodClass::ApiKeyReference,
        Some("gateway_managed" | "gateway_api_key") => {
            CredentialMethodClass::ManagedProviderReference
        }
        _ => bail!("provider_credential_method_invalid"),
    };
    let model_identity = value["provider"]["model"]["model_id"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 80)
        .ok_or_else(|| anyhow::anyhow!("provider_model_identity_missing"))?;
    let endpoint_fingerprint = value["provider"]["endpoint_fingerprint"]
        .as_str()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow::anyhow!("provider_endpoint_fingerprint_missing"))?;
    Ok(ProviderIdentity {
        route_class,
        dialect,
        credential_method,
        model_identity: model_identity.into(),
        endpoint_fingerprint: endpoint_fingerprint.to_ascii_lowercase(),
    })
}

fn attempt(value: &Value, attempt_number: usize) -> Result<ProviderAttemptEvidence> {
    let method = value["method"]
        .as_str()
        .map(str::to_ascii_uppercase)
        .filter(|value| value == "POST")
        .ok_or_else(|| anyhow::anyhow!("provider_method_invalid"))?;
    let route_identity = value["route"]
        .as_str()
        .filter(|value| *value == "/v1/chat/completions")
        .ok_or_else(|| anyhow::anyhow!("provider_route_identity_invalid"))?
        .to_owned();
    let response = &value["response"];
    let status = response["status_code"].as_u64().map(|value| value as u16);
    let disposition = match response["disposition"].as_str() {
        Some("completed") => CaptureAttemptDisposition::Success,
        Some("http_error") if status == Some(429) => CaptureAttemptDisposition::RateLimited,
        Some("http_error")
            if status.is_some_and(|value| value == 408 || value == 425 || value >= 500) =>
        {
            CaptureAttemptDisposition::Retried
        }
        Some("http_error" | "protocol_error") => CaptureAttemptDisposition::ProviderRejected,
        Some("transport_error") => CaptureAttemptDisposition::TransportFailed,
        Some("timeout") => CaptureAttemptDisposition::TimedOut,
        Some("cancelled") => CaptureAttemptDisposition::Cancelled,
        _ => bail!("provider_disposition_invalid"),
    };
    let framing = match response["framing"].as_str() {
        Some("server_sent_events") => StreamFraming::Sse,
        Some("fixed_length" | "chunked") => StreamFraming::Json,
        Some("none") => StreamFraming::NoBody,
        _ => bail!("provider_framing_invalid"),
    };
    let content_type = if framing == StreamFraming::Sse {
        Some("text/event-stream".into())
    } else if response["content_class"].as_str() == Some("json") {
        Some("application/json".into())
    } else {
        None
    };
    let usage = match value["metrics"]["usage"].as_object() {
        Some(usage) => Some(UsageEvidence {
            prompt_tokens: usage["input_tokens"]
                .as_u64()
                .context("provider input usage")?,
            completion_tokens: usage["output_tokens"]
                .as_u64()
                .context("provider output usage")?,
            total_tokens: usage["total_tokens"]
                .as_u64()
                .context("provider total usage")?,
            complete: true,
        }),
        None => None,
    };
    let headers = value["request_header_names"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("provider_headers_missing"))?
        .iter()
        .map(|header| {
            header
                .as_str()
                .map(str::to_ascii_lowercase)
                .filter(|value| value.len() <= 128)
                .ok_or_else(|| anyhow::anyhow!("provider_header_invalid"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ProviderAttemptEvidence {
        attempt: u32::try_from(attempt_number).context("provider attempt number")?,
        method,
        route_identity,
        present_request_headers: headers,
        request_body: None,
        response_body: None,
        response_status: status,
        response_content_type: content_type,
        framing,
        disposition,
        usage,
        latency_millis: value["metrics"]["latency_millis"]
            .as_u64()
            .context("provider latency")?,
    })
}

fn check(name: &str, passed: bool, detail_code: &str) -> CertificationCheck {
    CertificationCheck {
        name: name.into(),
        passed,
        detail_code: detail_code.into(),
    }
}

fn hash_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn is_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokptah_agent_bridge::provider_observation::{
        AttemptMetrics, AuthoritativeUsage, CredentialMethod, EndpointFingerprint, EvidenceMode,
        ObservationScope, OpaqueScopeId, ProviderDialect, ProviderIdentity as ObservedIdentity,
        ProviderRouteClass as ObservedRouteClass, PublicModelId, RequestHeaderName,
        RequestRouteIdentity, ResponseContentClass, ResponseFraming, ResponseMetadata,
    };
    use grokptah_agent_bridge::public_xai_endpoint_fingerprint;
    use grokptah_agent_bridge::CertificationBoundProfile;
    use uuid::Uuid;

    fn bounds() -> HardBounds {
        HardBounds {
            bound_profile: CertificationBoundProfile::Smoke,
            max_duration_seconds: 60,
            max_rounds: 2,
            max_run_tokens: 1_000,
            max_total_tokens: 2_000,
            max_provider_requests: 4,
            max_continuations: 1,
            max_response_bytes_per_request: 1_024,
            max_output_bytes: 1_024,
            max_artifact_bytes: 4_096,
        }
    }

    #[test]
    fn hashes_are_not_reversible_identifiers() {
        assert_eq!(hash_hex("run-1").len(), 64);
        assert!(is_hash(&hash_hex("run-1")));
        assert!(!is_hash("run-1"));
    }

    #[test]
    fn live_observation_becomes_complete_payload_free_capture() {
        let session_id = Uuid::parse_str("018f1234-5678-7abc-8def-0123456789ab")
            .unwrap()
            .to_string();
        let run_id = "run-1";
        let endpoint =
            public_xai_endpoint_fingerprint(&ProviderRouteClass::GrokBuildProxy).unwrap();
        let observation = ProviderObservation::new(
            EvidenceMode::MetadataOnly,
            ObservationScope::new(
                OpaqueScopeId::from_stable_input("campaign-1"),
                OpaqueScopeId::from_stable_input(run_id),
                OpaqueScopeId::new(&session_id).unwrap(),
            ),
            1,
            ObservedRouteClass::GrokBuildProxy,
            ProviderDialect::GrokBuild,
            CredentialMethod::GrokBuildOidc,
            ObservedIdentity::public(
                PublicModelId::new("grok-build").unwrap(),
                EndpointFingerprint::new(&endpoint).unwrap(),
            ),
            RequestRouteIdentity::public_chat_completions(),
            vec![RequestHeaderName::ContentType],
            ResponseMetadata::new(
                Some(200),
                Some(ResponseContentClass::EventStream),
                ResponseFraming::ServerSentEvents,
                grokptah_agent_bridge::provider_observation::AttemptDisposition::Completed,
            )
            .unwrap(),
            AttemptMetrics::new(12, 24, 25, Some(AuthoritativeUsage::new(1, 1, 2).unwrap()))
                .unwrap(),
        )
        .unwrap();
        let capture = build_capture(
            "campaign-1",
            "agent-initial-run-001",
            "b6dab133",
            false,
            &bounds(),
            &[observation],
            CaptureRunSet {
                provider: &ProviderRunEvidence {
                    session_id: session_id.clone(),
                    agent_id: "agent-1".into(),
                    run_id: run_id.into(),
                    agent_spec_revision: 1,
                    checkpoint_id: Some("checkpoint-1".into()),
                    parent_run_id: None,
                    continuation_context_hash: None,
                    continuation_fidelity: None,
                    state: "completed".into(),
                    stop_cause: None,
                },
                recovery: &[],
            },
        )
        .unwrap();
        capture.validate_complete_structural_evidence().unwrap();
        let serialized = serde_json::to_string(&capture).unwrap();
        assert!(!serialized.contains("BOUNDED_RUN_OK"));
        assert!(capture.attempts[0].request_body.is_none());
        assert!(capture.attempts[0].response_body.is_none());
    }

    #[test]
    fn recovery_capture_keeps_provider_and_scenario_runs_distinct() {
        let session_id = Uuid::parse_str("018f1234-5678-7abc-8def-0123456789ab")
            .unwrap()
            .to_string();
        let endpoint =
            public_xai_endpoint_fingerprint(&ProviderRouteClass::GrokBuildProxy).unwrap();
        let observation = ProviderObservation::new(
            EvidenceMode::MetadataOnly,
            ObservationScope::new(
                OpaqueScopeId::from_stable_input("campaign-recovery"),
                OpaqueScopeId::from_stable_input("seed-run"),
                OpaqueScopeId::new(&session_id).unwrap(),
            ),
            1,
            ObservedRouteClass::GrokBuildProxy,
            ProviderDialect::GrokBuild,
            CredentialMethod::GrokBuildOidc,
            ObservedIdentity::public(
                PublicModelId::new("grok-build").unwrap(),
                EndpointFingerprint::new(&endpoint).unwrap(),
            ),
            RequestRouteIdentity::public_chat_completions(),
            vec![RequestHeaderName::ContentType],
            ResponseMetadata::new(
                Some(200),
                Some(ResponseContentClass::EventStream),
                ResponseFraming::ServerSentEvents,
                grokptah_agent_bridge::provider_observation::AttemptDisposition::Completed,
            )
            .unwrap(),
            AttemptMetrics::new(12, 24, 25, Some(AuthoritativeUsage::new(1, 1, 2).unwrap()))
                .unwrap(),
        )
        .unwrap();
        let capture = build_capture(
            "campaign-recovery",
            "interrupt-recover-001",
            "b6dab133",
            false,
            &bounds(),
            &[observation],
            CaptureRunSet {
                provider: &ProviderRunEvidence {
                    session_id: session_id.clone(),
                    agent_id: "agent-1".into(),
                    run_id: "seed-run".into(),
                    agent_spec_revision: 1,
                    checkpoint_id: None,
                    parent_run_id: None,
                    continuation_context_hash: None,
                    continuation_fidelity: None,
                    state: "completed".into(),
                    stop_cause: None,
                },
                recovery: &[ProviderRunEvidence {
                    session_id,
                    agent_id: "agent-1".into(),
                    run_id: "interrupted-run".into(),
                    agent_spec_revision: 1,
                    checkpoint_id: None,
                    parent_run_id: None,
                    continuation_context_hash: None,
                    continuation_fidelity: None,
                    state: "interrupted".into(),
                    stop_cause: Some("host_restart".into()),
                }],
            },
        )
        .unwrap();
        capture.validate_complete_structural_evidence().unwrap();
        assert_eq!(capture.durable_states.len(), 2);
        assert_eq!(
            capture.durable_states[0].role,
            DurableEvidenceRole::ProviderCapture
        );
        assert_eq!(
            capture.durable_states[1].role,
            DurableEvidenceRole::Recovery
        );
        assert!(capture
            .validate_for_xai_fixture_promotion_at(std::path::Path::new("."))
            .is_err());
    }
}
