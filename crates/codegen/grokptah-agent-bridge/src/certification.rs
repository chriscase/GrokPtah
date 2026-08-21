//! Safe evidence contract for persistent-Agent provider certification.
//!
//! Live campaigns may be stochastic, but their retained evidence must be
//! bounded, secret-free, and useful to deterministic replay tests. This
//! module deliberately stores hashes and structural metadata instead of
//! credentials, arbitrary headers, endpoint URLs, or model prose.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const PERSISTENT_AGENT_CAPTURE_SCHEMA: &str = "grokptah.persistent_agent_capture.v2";
pub const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CAPTURE_ATTEMPTS: usize = 4_096;
pub const MAX_CAPTURE_CHECKS: usize = 2_048;
pub const MAX_CAPTURE_STRING_BYTES: usize = 8 * 1024;
pub const MAX_RAW_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_PROMOTABLE_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;
pub const PERSISTENT_AGENT_SCENARIO_IDS: &[&str] = &[
    "xai-route-oidc-001",
    "sse-stream-001",
    "native-tools-001",
    "retry-transient-001",
    "agent-initial-run-001",
    "restart-between-runs-001",
    "resume-same-lane-001",
    "resume-cross-lane-001",
    "archive-lane-001",
    "interrupt-recover-001",
    "resume-idempotency-001",
    "managed-work-run-001",
    "memory-scopes-001",
    "spec-revision-001",
    "token-ceiling-001",
    "endurance-finite-runs-001",
];
const CERTIFICATION_CATALOG: &str =
    include_str!("../../../../evals/persistent-agent-scenarios.v1.json");
const GROK_BUILD_PROXY_BASE: &str = "https://cli-chat-proxy.grok.com/v1";
const XAI_API_BASE: &str = "https://api.x.ai/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRouteClass {
    GrokBuildProxy,
    XaiApi,
    CompatibleGateway,
}

impl ProviderRouteClass {
    pub fn is_public_xai(&self) -> bool {
        matches!(self, Self::GrokBuildProxy | Self::XaiApi)
    }
}

/// Stable allowlisted fingerprint for a public xAI route. Captures retain
/// this digest rather than the endpoint URL itself.
pub fn public_xai_endpoint_fingerprint(route_class: &ProviderRouteClass) -> Option<String> {
    let endpoint = match route_class {
        ProviderRouteClass::GrokBuildProxy => GROK_BUILD_PROXY_BASE,
        ProviderRouteClass::XaiApi => XAI_API_BASE,
        ProviderRouteClass::CompatibleGateway => return None,
    };
    Some(format!("{:x}", Sha256::digest(endpoint.as_bytes())))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMethodClass {
    GrokBuildOidc,
    ApiKeyReference,
    ManagedProviderReference,
}

/// Authoritative bound profiles shared by captures and certification runners.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CertificationBoundProfile {
    Smoke,
    Standard,
    Extended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificationBoundLimits {
    pub per_run_tokens: u64,
    pub campaign_tokens: u64,
    pub provider_requests: u32,
    pub continuations: u32,
    pub duration_seconds: u64,
    pub artifact_bytes: u64,
    pub response_bytes: u64,
}

impl CertificationBoundProfile {
    pub const fn limits(self) -> CertificationBoundLimits {
        match self {
            Self::Smoke => CertificationBoundLimits {
                per_run_tokens: 20_000,
                campaign_tokens: 100_000,
                provider_requests: 40,
                continuations: 4,
                duration_seconds: 1_800,
                artifact_bytes: 128 * 1024 * 1024,
                response_bytes: 8 * 1024 * 1024,
            },
            Self::Standard => CertificationBoundLimits {
                per_run_tokens: 100_000,
                campaign_tokens: 500_000,
                provider_requests: 160,
                continuations: 16,
                duration_seconds: 7_200,
                artifact_bytes: 512 * 1024 * 1024,
                response_bytes: 16 * 1024 * 1024,
            },
            Self::Extended => CertificationBoundLimits {
                per_run_tokens: 250_000,
                campaign_tokens: 2_000_000,
                provider_requests: 800,
                continuations: 96,
                duration_seconds: 86_400,
                artifact_bytes: 2 * 1024 * 1024 * 1024,
                response_bytes: 32 * 1024 * 1024,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDialectClass {
    XaiChatCompletions,
    OpenAiCompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamFraming {
    Sse,
    Json,
    NoBody,
    TransportFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptDisposition {
    Success,
    Retried,
    Downgraded,
    RateLimited,
    TimedOut,
    TransportFailed,
    ProviderRejected,
    Cancelled,
}

/// Identifies why a durable state is present in a provider capture.
///
/// `Primary` is the backwards-compatible default for the original single-Run
/// capture shape. Recovery captures explicitly separate the completed Run
/// whose provider observations are retained from the interrupted/retried Run
/// whose durable lifecycle is being certified.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DurableEvidenceRole {
    #[default]
    Primary,
    ProviderCapture,
    Recovery,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CampaignIdentity {
    /// Opaque SHA-256 label for the owning lab campaign. The raw campaign ID
    /// is deliberately not part of portable provider evidence.
    pub campaign_id: String,
    pub scenario_id: String,
    pub repository_commit: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderIdentity {
    pub route_class: ProviderRouteClass,
    pub dialect: ProviderDialectClass,
    pub credential_method: CredentialMethodClass,
    /// Public xAI model ID, or a stable `opaque-<hash>` label for a private
    /// compatible gateway. Never store a private catalog value here.
    pub model_identity: String,
    /// Hash of the normalized endpoint. The endpoint itself is never stored.
    pub endpoint_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CampaignBudgets {
    pub bound_profile: CertificationBoundProfile,
    pub max_run_tokens: u64,
    pub max_total_tokens: u64,
    pub max_provider_requests: u32,
    pub max_continuations: u32,
    pub max_duration_seconds: u64,
    pub max_raw_artifact_bytes: u64,
    pub max_response_bytes_per_request: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CampaignActuals {
    pub total_tokens: u64,
    pub provider_requests: u32,
    pub continuations: u32,
    pub duration_seconds: u64,
    pub raw_artifact_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageEvidence {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactReference {
    /// Relative to one ignored campaign directory or one versioned fixture
    /// directory. Absolute paths and parent traversal are forbidden.
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAttemptEvidence {
    pub attempt: u32,
    pub method: String,
    /// Public normalized route template such as `/v1/chat/completions`.
    /// Private compatible-gateway paths must use an opaque label.
    pub route_identity: String,
    /// Lowercase header names only. Values are never retained.
    pub present_request_headers: Vec<String>,
    pub request_body: Option<ArtifactReference>,
    pub response_body: Option<ArtifactReference>,
    pub response_status: Option<u16>,
    pub response_content_type: Option<String>,
    pub framing: StreamFraming,
    pub disposition: AttemptDisposition,
    pub usage: Option<UsageEvidence>,
    pub latency_millis: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableStateEvidence {
    /// Defaults to `primary` when reading the original v2 single-Run shape.
    #[serde(default)]
    pub role: DurableEvidenceRole,
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub lane_id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub continuation_input_hash: Option<String>,
    pub continuation_context_hash: Option<String>,
    pub continuation_fidelity: Option<String>,
    pub terminal_state: String,
    pub stop_cause: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CertificationCheck {
    pub name: String,
    pub passed: bool,
    /// Bounded host-authored classification. Never store model prose.
    pub detail_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersistentAgentCapture {
    pub schema: String,
    pub campaign: CampaignIdentity,
    pub provider: ProviderIdentity,
    pub budgets: CampaignBudgets,
    pub actuals: CampaignActuals,
    pub attempts: Vec<ProviderAttemptEvidence>,
    pub durable_states: Vec<DurableStateEvidence>,
    pub checks: Vec<CertificationCheck>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CertificationError {
    #[error("capture schema is unsupported")]
    UnsupportedSchema,
    #[error("capture exceeds a structural bound: {0}")]
    Bound(&'static str),
    #[error("capture contains an invalid identifier: {0}")]
    Identifier(&'static str),
    #[error("capture contains an unsafe artifact reference")]
    UnsafeArtifactReference,
    #[error("capture contains forbidden data at {path}: {rule}")]
    ForbiddenData { path: String, rule: &'static str },
    #[error("capture is not eligible for public fixture promotion: {0}")]
    NotPromotable(&'static str),
    #[error("capture JSON serialization failed")]
    Serialization,
    #[error("capture artifact could not be verified: {0}")]
    ArtifactVerification(&'static str),
}

impl PersistentAgentCapture {
    pub fn validate(&self) -> Result<(), CertificationError> {
        if self.schema != PERSISTENT_AGENT_CAPTURE_SCHEMA {
            return Err(CertificationError::UnsupportedSchema);
        }
        validate_opaque_label(&self.campaign.campaign_id, "campaign_id")?;
        validate_identifier(&self.campaign.scenario_id, "scenario_id")?;
        validate_commit(&self.campaign.repository_commit)?;
        validate_identity(&self.provider)?;
        validate_budgets(&self.budgets)?;
        if self.attempts.len() > MAX_CAPTURE_ATTEMPTS {
            return Err(CertificationError::Bound("attempt count"));
        }
        if self.attempts.len() > self.budgets.max_provider_requests as usize {
            return Err(CertificationError::Bound("attempts exceed campaign budget"));
        }
        if self.checks.len() > MAX_CAPTURE_CHECKS {
            return Err(CertificationError::Bound("check count"));
        }
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.attempt as usize != index + 1 {
                return Err(CertificationError::Identifier("attempt sequence"));
            }
            validate_attempt(attempt, &self.provider.route_class)?;
            if attempt
                .usage
                .as_ref()
                .is_some_and(|usage| usage.total_tokens > self.budgets.max_run_tokens)
            {
                return Err(CertificationError::Bound(
                    "attempt usage exceeds per-run token budget",
                ));
            }
            if attempt
                .response_body
                .as_ref()
                .is_some_and(|body| body.bytes > self.budgets.max_response_bytes_per_request)
            {
                return Err(CertificationError::Bound(
                    "response artifact exceeds per-request budget",
                ));
            }
        }
        if self
            .attempts
            .last()
            .is_some_and(|attempt| attempt.disposition == AttemptDisposition::Retried)
        {
            return Err(CertificationError::Identifier(
                "retried attempt has no following attempt",
            ));
        }
        for state in &self.durable_states {
            // Capture v2 is portable evidence, not a local operational log.
            // Every durable identity must already be irreversibly scoped before
            // serialization so direct validation/promotion cannot bypass the
            // report layer's opaque-ID requirement.
            validate_opaque_label(&state.agent_id, "agent_id")?;
            validate_opaque_label(&state.lane_id, "lane_id")?;
            validate_opaque_label(&state.run_id, "run_id")?;
            validate_optional_opaque_label(state.parent_run_id.as_deref(), "parent_run_id")?;
            validate_optional_opaque_label(state.checkpoint_id.as_deref(), "checkpoint_id")?;
            validate_optional_hash(
                state.continuation_input_hash.as_deref(),
                "continuation_input_hash",
            )?;
            validate_optional_hash(
                state.continuation_context_hash.as_deref(),
                "continuation_context_hash",
            )?;
            validate_short_token(&state.terminal_state, "terminal_state")?;
            if let Some(value) = &state.continuation_fidelity {
                validate_short_token(value, "continuation_fidelity")?;
            }
            if let Some(value) = &state.stop_cause {
                validate_short_token(value, "stop_cause")?;
            }
        }
        let mut check_names = BTreeSet::new();
        for check in &self.checks {
            validate_identifier(&check.name, "check name")?;
            validate_short_token(&check.detail_code, "check detail_code")?;
            if !check_names.insert(check.name.as_str()) {
                return Err(CertificationError::Identifier("duplicate check name"));
            }
        }
        validate_actuals(self)?;
        let value = serde_json::to_value(self).map_err(|_| CertificationError::Serialization)?;
        scan_value_for_forbidden_data(&value)?;
        let bytes = serde_json::to_vec(self).map_err(|_| CertificationError::Serialization)?;
        if bytes.len() > MAX_CAPTURE_BYTES {
            return Err(CertificationError::Bound("serialized capture bytes"));
        }
        Ok(())
    }

    /// Apply the stricter, root-bound gate used before a capture can become a
    /// committed xAI replay fixture. This opens every referenced artifact,
    /// verifies its size and digest, and scans its structured contents. It
    /// does not write files; promotion remains an explicit reviewed operation.
    pub fn validate_for_xai_fixture_promotion_at(
        &self,
        fixture_root: &Path,
    ) -> Result<(), CertificationError> {
        self.validate()?;
        if !self.provider.route_class.is_public_xai() {
            return Err(CertificationError::NotPromotable(
                "private compatible-gateway captures are metadata-only",
            ));
        }
        if self.campaign.dirty {
            return Err(CertificationError::NotPromotable(
                "campaign repository was dirty",
            ));
        }
        if self.attempts.is_empty() {
            return Err(CertificationError::NotPromotable(
                "capture has no provider attempts",
            ));
        }
        self.validate_complete_structural_evidence()?;
        let expected_endpoint = public_xai_endpoint_fingerprint(&self.provider.route_class).ok_or(
            CertificationError::NotPromotable("provider route is not a public xAI route"),
        )?;
        if self.provider.endpoint_fingerprint != expected_endpoint {
            return Err(CertificationError::NotPromotable(
                "provider endpoint is not on the public xAI allowlist",
            ));
        }
        if !PERSISTENT_AGENT_SCENARIO_IDS.contains(&self.campaign.scenario_id.as_str()) {
            return Err(CertificationError::NotPromotable(
                "campaign scenario is not in the versioned catalog",
            ));
        }
        if self
            .durable_states
            .iter()
            .any(|state| state.role == DurableEvidenceRole::Recovery)
        {
            return Err(CertificationError::NotPromotable(
                "recovery captures require manual review before fixture promotion",
            ));
        }
        verify_promotion_artifacts(self, fixture_root)?;
        Ok(())
    }

    /// Validate the complete structural provider/durable oracle needed by a
    /// report or normalization candidate. This does not approve promotion and
    /// does not inspect payload artifacts; callers must apply their own
    /// root-bound artifact policy.
    pub fn validate_complete_structural_evidence(&self) -> Result<(), CertificationError> {
        self.validate()?;
        let provider_captures = self
            .durable_states
            .iter()
            .filter(|state| state.role == DurableEvidenceRole::ProviderCapture)
            .count();
        let recovery_states = self
            .durable_states
            .iter()
            .filter(|state| state.role == DurableEvidenceRole::Recovery)
            .count();
        let legacy_single_run = self.durable_states.len() == 1
            && self.durable_states[0].role == DurableEvidenceRole::Primary;
        let recovery_partition = provider_captures == 1 && recovery_states > 0;
        if !legacy_single_run && !recovery_partition {
            return Err(CertificationError::NotPromotable(
                "complete provider evidence must use one primary Run or an explicit provider/recovery partition",
            ));
        }
        if !self.provider.route_class.is_public_xai() {
            return Err(CertificationError::NotPromotable(
                "provider evidence is not an allowlisted public xAI route",
            ));
        }
        let expected_endpoint = public_xai_endpoint_fingerprint(&self.provider.route_class).ok_or(
            CertificationError::NotPromotable("provider route is not a public xAI route"),
        )?;
        if self.provider.endpoint_fingerprint != expected_endpoint {
            return Err(CertificationError::NotPromotable(
                "provider endpoint is not on the public xAI allowlist",
            ));
        }
        if !PERSISTENT_AGENT_SCENARIO_IDS.contains(&self.campaign.scenario_id.as_str()) {
            return Err(CertificationError::NotPromotable(
                "campaign scenario is not in the versioned catalog",
            ));
        }
        if recovery_partition {
            let provider = self
                .durable_states
                .iter()
                .find(|state| state.role == DurableEvidenceRole::ProviderCapture)
                .expect("provider capture count checked above");
            if !matches!(provider.terminal_state.as_str(), "completed" | "succeeded") {
                return Err(CertificationError::NotPromotable(
                    "provider-capture Run is not terminal-successful",
                ));
            }
            for state in self
                .durable_states
                .iter()
                .filter(|state| state.role == DurableEvidenceRole::Recovery)
            {
                if !matches!(
                    state.terminal_state.as_str(),
                    "interrupted" | "failed" | "completed" | "succeeded" | "limit_reached"
                ) {
                    return Err(CertificationError::NotPromotable(
                        "recovery Run has an unsupported terminal state",
                    ));
                }
            }
            return validate_recovery_completeness(self);
        }
        validate_promotion_completeness(self)
    }
}

fn validate_recovery_completeness(
    capture: &PersistentAgentCapture,
) -> Result<(), CertificationError> {
    let required_checks = [
        "provider_observation_complete",
        "provider_attempts_bound_to_durable_run",
        "recovery_state_partitioned",
        "no_implicit_invocation_resume",
    ];
    let checks: BTreeSet<&str> = capture
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    if capture.checks.is_empty()
        || capture.checks.iter().any(|check| !check.passed)
        || required_checks.iter().any(|check| !checks.contains(check))
    {
        return Err(CertificationError::NotPromotable(
            "recovery capture lacks passing partition and no-resume checks",
        ));
    }
    if capture
        .attempts
        .iter()
        .any(|attempt| attempt.usage.as_ref().is_none_or(|usage| !usage.complete))
    {
        return Err(CertificationError::NotPromotable(
            "recovery capture has incomplete provider usage",
        ));
    }
    Ok(())
}

fn validate_actuals(capture: &PersistentAgentCapture) -> Result<(), CertificationError> {
    if capture.actuals.provider_requests as usize != capture.attempts.len() {
        return Err(CertificationError::Identifier(
            "provider request actual does not match attempts",
        ));
    }
    if capture.actuals.provider_requests > capture.budgets.max_provider_requests {
        return Err(CertificationError::Bound("provider request actual"));
    }

    let total_tokens = capture.attempts.iter().try_fold(0_u64, |total, attempt| {
        total
            .checked_add(attempt.usage.as_ref().map_or(0, |usage| usage.total_tokens))
            .ok_or(CertificationError::Bound("aggregate token usage overflow"))
    })?;
    if total_tokens != capture.actuals.total_tokens {
        return Err(CertificationError::Identifier(
            "token actual does not match attempt usage",
        ));
    }
    if capture.actuals.total_tokens > capture.budgets.max_total_tokens {
        return Err(CertificationError::Bound("aggregate token usage"));
    }
    if capture.durable_states.len() == 1
        && capture.actuals.total_tokens > capture.budgets.max_run_tokens
    {
        return Err(CertificationError::Bound("per-Run aggregate token usage"));
    }

    let continuations = capture
        .durable_states
        .iter()
        .filter(|state| state.parent_run_id.is_some())
        .count();
    if capture.actuals.continuations as usize != continuations {
        return Err(CertificationError::Identifier(
            "continuation actual does not match durable states",
        ));
    }
    if capture.actuals.continuations > capture.budgets.max_continuations {
        return Err(CertificationError::Bound("continuation actual"));
    }
    if capture.actuals.duration_seconds > capture.budgets.max_duration_seconds {
        return Err(CertificationError::Bound("campaign duration actual"));
    }

    let mut artifacts = BTreeMap::<&str, (&str, u64)>::new();
    for reference in capture
        .attempts
        .iter()
        .flat_map(|attempt| {
            [
                attempt.request_body.as_ref(),
                attempt.response_body.as_ref(),
            ]
        })
        .flatten()
    {
        match artifacts.get(reference.relative_path.as_str()) {
            Some((sha256, bytes)) if *sha256 != reference.sha256 || *bytes != reference.bytes => {
                return Err(CertificationError::Identifier(
                    "artifact path has conflicting evidence",
                ));
            }
            Some(_) => {}
            None => {
                artifacts.insert(
                    reference.relative_path.as_str(),
                    (reference.sha256.as_str(), reference.bytes),
                );
            }
        }
    }
    let referenced_bytes = artifacts.values().try_fold(0_u64, |total, (_, bytes)| {
        total.checked_add(*bytes).ok_or(CertificationError::Bound(
            "aggregate artifact bytes overflow",
        ))
    })?;
    if referenced_bytes != capture.actuals.raw_artifact_bytes {
        return Err(CertificationError::Identifier(
            "artifact actual does not match referenced artifacts",
        ));
    }
    if capture.actuals.raw_artifact_bytes > capture.budgets.max_raw_artifact_bytes {
        return Err(CertificationError::Bound("aggregate artifact bytes"));
    }
    Ok(())
}

fn validate_identity(identity: &ProviderIdentity) -> Result<(), CertificationError> {
    validate_hash(&identity.endpoint_fingerprint, "endpoint_fingerprint")?;
    validate_short_token(&identity.model_identity, "model_identity")?;
    if matches!(identity.route_class, ProviderRouteClass::CompatibleGateway)
        && !identity.model_identity.starts_with("opaque-")
    {
        return Err(CertificationError::Identifier(
            "private model identity must be opaque",
        ));
    }
    if identity.route_class.is_public_xai()
        && (!(6..=80).contains(&identity.model_identity.len())
            || !identity.model_identity.starts_with("grok-")
            || !identity.model_identity.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'.' | b'_')
            }))
    {
        return Err(CertificationError::Identifier("public xAI model identity"));
    }
    if identity.route_class.is_public_xai()
        && !matches!(identity.dialect, ProviderDialectClass::XaiChatCompletions)
    {
        return Err(CertificationError::Identifier(
            "public xAI route requires xAI chat-completions dialect",
        ));
    }
    match (&identity.route_class, &identity.credential_method) {
        (ProviderRouteClass::GrokBuildProxy, CredentialMethodClass::GrokBuildOidc)
        | (ProviderRouteClass::XaiApi, CredentialMethodClass::ApiKeyReference)
        | (
            ProviderRouteClass::CompatibleGateway,
            CredentialMethodClass::ManagedProviderReference,
        ) => Ok(()),
        _ => Err(CertificationError::Identifier(
            "provider route and credential method",
        )),
    }
}

fn validate_budgets(budgets: &CampaignBudgets) -> Result<(), CertificationError> {
    if budgets.max_run_tokens == 0
        || budgets.max_total_tokens == 0
        || budgets.max_provider_requests == 0
        || budgets.max_continuations == 0
        || budgets.max_duration_seconds == 0
        || budgets.max_raw_artifact_bytes == 0
        || budgets.max_response_bytes_per_request == 0
    {
        return Err(CertificationError::Bound("zero campaign budget"));
    }
    let limits = budgets.bound_profile.limits();
    if budgets.max_run_tokens > limits.per_run_tokens
        || budgets.max_total_tokens > limits.campaign_tokens
        || budgets.max_run_tokens > budgets.max_total_tokens
        || budgets.max_provider_requests > limits.provider_requests
        || budgets.max_continuations > limits.continuations
        || budgets.max_duration_seconds > limits.duration_seconds
        || budgets.max_raw_artifact_bytes > limits.artifact_bytes
        || budgets.max_response_bytes_per_request > limits.response_bytes
    {
        return Err(CertificationError::Bound(
            "campaign budgets exceed bound profile",
        ));
    }
    if budgets.max_raw_artifact_bytes > MAX_RAW_ARTIFACT_BYTES {
        return Err(CertificationError::Bound("raw artifact bytes"));
    }
    if budgets.max_provider_requests as usize > MAX_CAPTURE_ATTEMPTS {
        return Err(CertificationError::Bound("provider request budget"));
    }
    if budgets.max_continuations as usize > MAX_CAPTURE_ATTEMPTS {
        return Err(CertificationError::Bound("continuation budget"));
    }
    if budgets.max_response_bytes_per_request > budgets.max_raw_artifact_bytes {
        return Err(CertificationError::Bound(
            "response budget exceeds campaign artifact budget",
        ));
    }
    Ok(())
}

fn validate_attempt(
    attempt: &ProviderAttemptEvidence,
    route_class: &ProviderRouteClass,
) -> Result<(), CertificationError> {
    if attempt.attempt == 0 {
        return Err(CertificationError::Identifier("attempt number"));
    }
    if !matches!(attempt.method.as_str(), "GET" | "POST") {
        return Err(CertificationError::Identifier("HTTP method"));
    }
    if route_class.is_public_xai() {
        if attempt.method != "POST" || attempt.route_identity != "/v1/chat/completions" {
            return Err(CertificationError::Identifier("public xAI request route"));
        }
    } else if !attempt.route_identity.starts_with("opaque-") {
        return Err(CertificationError::Identifier(
            "private route identity must be opaque",
        ));
    }
    validate_short_string(&attempt.route_identity, "route_identity")?;
    let mut headers = BTreeSet::new();
    for header in &attempt.present_request_headers {
        validate_header_name(header)?;
        if !headers.insert(header) {
            return Err(CertificationError::Identifier("duplicate header name"));
        }
    }
    if let Some(reference) = &attempt.request_body {
        validate_artifact(reference)?;
    }
    if let Some(reference) = &attempt.response_body {
        validate_artifact(reference)?;
    }
    if let Some(content_type) = &attempt.response_content_type {
        validate_content_type(content_type)?;
    }
    if let Some(usage) = &attempt.usage {
        let summed = usage
            .prompt_tokens
            .checked_add(usage.completion_tokens)
            .ok_or(CertificationError::Bound("usage overflow"))?;
        if usage.complete && usage.total_tokens < summed {
            return Err(CertificationError::Identifier("complete token usage"));
        }
    }
    let status = attempt.response_status;
    let response_shape_valid = match attempt.disposition {
        AttemptDisposition::Success | AttemptDisposition::Downgraded => {
            status.is_some_and(|value| (200..300).contains(&value))
                && matches!(attempt.framing, StreamFraming::Sse | StreamFraming::Json)
        }
        AttemptDisposition::RateLimited => {
            status == Some(429) && attempt.framing != StreamFraming::TransportFailure
        }
        AttemptDisposition::ProviderRejected => {
            status.is_some_and(|value| (400..600).contains(&value) && value != 429)
                && attempt.framing != StreamFraming::TransportFailure
        }
        AttemptDisposition::Retried => {
            status.is_some_and(|value| {
                value == 408 || value == 425 || value == 429 || (500..600).contains(&value)
            }) && attempt.framing != StreamFraming::TransportFailure
        }
        AttemptDisposition::TransportFailed => {
            status.is_none() && attempt.framing == StreamFraming::TransportFailure
        }
        AttemptDisposition::TimedOut | AttemptDisposition::Cancelled => {
            status.is_none() && attempt.framing == StreamFraming::NoBody
        }
    };
    if !response_shape_valid
        || (attempt.framing == StreamFraming::Sse
            && attempt.response_content_type.as_deref() != Some("text/event-stream"))
        || (attempt.framing == StreamFraming::TransportFailure
            && attempt.response_content_type.is_some())
    {
        return Err(CertificationError::Identifier(
            "response status/disposition/framing consistency",
        ));
    }
    Ok(())
}

fn validate_promotion_completeness(
    capture: &PersistentAgentCapture,
) -> Result<(), CertificationError> {
    if capture.checks.is_empty() || capture.checks.iter().any(|check| !check.passed) {
        return Err(CertificationError::NotPromotable(
            "certification checks are empty or not all passing",
        ));
    }
    let required_checks = required_live_checks(&capture.campaign.scenario_id)?;
    let observed: BTreeSet<&str> = capture
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    for required in [
        "provider_observation_complete",
        "provider_attempts_bound_to_durable_run",
    ] {
        if !observed.contains(required) {
            return Err(CertificationError::NotPromotable(
                "provider observation or Run binding evidence is missing",
            ));
        }
    }
    if required_checks
        .iter()
        .any(|check| !observed.contains(check.as_str()))
    {
        return Err(CertificationError::NotPromotable(
            "scenario-required live checks are missing",
        ));
    }
    if capture
        .attempts
        .iter()
        .any(|attempt| attempt.usage.as_ref().is_none_or(|usage| !usage.complete))
    {
        return Err(CertificationError::NotPromotable(
            "authoritative usage is incomplete",
        ));
    }
    for (index, attempt) in capture.attempts.iter().enumerate() {
        let final_attempt = index + 1 == capture.attempts.len();
        let disposition_is_causal = if final_attempt {
            matches!(
                attempt.disposition,
                AttemptDisposition::Success | AttemptDisposition::Downgraded
            )
        } else {
            matches!(
                attempt.disposition,
                AttemptDisposition::Retried
                    | AttemptDisposition::RateLimited
                    | AttemptDisposition::TimedOut
                    | AttemptDisposition::TransportFailed
            )
        };
        if !disposition_is_causal {
            return Err(CertificationError::NotPromotable(
                "provider attempt sequence is not causally retryable with one successful terminal attempt",
            ));
        }
    }
    let terminal = capture
        .durable_states
        .last()
        .ok_or(CertificationError::NotPromotable(
            "capture has no durable terminal evidence",
        ))?;
    let terminal_ok = if capture.campaign.scenario_id == "token-ceiling-001" {
        terminal.terminal_state == "limit_reached"
            && terminal.stop_cause.as_deref().is_some_and(|cause| {
                matches!(
                    cause,
                    "token_ceiling"
                        | "max_total_tokens_reached"
                        | "max_total_tokens_usage_unavailable"
                )
            })
    } else {
        matches!(terminal.terminal_state.as_str(), "completed" | "succeeded")
    };
    if !terminal_ok {
        return Err(CertificationError::NotPromotable(
            "scenario durable terminal evidence is unsuccessful or inconsistent",
        ));
    }
    Ok(())
}

fn required_live_checks(scenario_id: &str) -> Result<Vec<String>, CertificationError> {
    let catalog: Value = serde_json::from_str(CERTIFICATION_CATALOG)
        .map_err(|_| CertificationError::NotPromotable("versioned catalog is malformed"))?;
    let scenario = catalog["scenarios"]
        .as_array()
        .and_then(|scenarios| {
            scenarios
                .iter()
                .find(|scenario| scenario["id"].as_str() == Some(scenario_id))
        })
        .ok_or(CertificationError::NotPromotable(
            "campaign scenario is not in the versioned catalog",
        ))?;
    scenario["live_checks"]
        .as_array()
        .ok_or(CertificationError::NotPromotable(
            "scenario live checks are malformed",
        ))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(CertificationError::NotPromotable(
                    "scenario live checks are malformed",
                ))
        })
        .collect()
}

fn validate_artifact(reference: &ArtifactReference) -> Result<(), CertificationError> {
    let path = Path::new(&reference.relative_path);
    if reference.relative_path.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(CertificationError::UnsafeArtifactReference);
    }
    validate_hash(&reference.sha256, "artifact sha256")?;
    if reference.bytes == 0 || reference.bytes > MAX_RAW_ARTIFACT_BYTES {
        return Err(CertificationError::Bound("artifact bytes"));
    }
    Ok(())
}

fn verify_promotion_artifacts(
    capture: &PersistentAgentCapture,
    fixture_root: &Path,
) -> Result<(), CertificationError> {
    let root = dunce::canonicalize(fixture_root)
        .map_err(|_| CertificationError::ArtifactVerification("fixture root is unavailable"))?;
    if !root
        .metadata()
        .map_err(|_| CertificationError::ArtifactVerification("fixture root is unavailable"))?
        .is_dir()
    {
        return Err(CertificationError::ArtifactVerification(
            "fixture root is not a directory",
        ));
    }

    let mut verified = BTreeSet::new();
    for reference in capture
        .attempts
        .iter()
        .flat_map(|attempt| {
            [
                attempt.request_body.as_ref(),
                attempt.response_body.as_ref(),
            ]
        })
        .flatten()
    {
        if verified.insert(reference.relative_path.as_str()) {
            verify_promotion_artifact(&root, reference)?;
        }
    }
    Ok(())
}

fn verify_promotion_artifact(
    canonical_root: &Path,
    reference: &ArtifactReference,
) -> Result<(), CertificationError> {
    validate_artifact(reference)?;
    if reference.bytes > MAX_PROMOTABLE_ARTIFACT_BYTES {
        return Err(CertificationError::Bound(
            "promotable artifact exceeds its memory-safe byte bound",
        ));
    }
    let relative = Path::new(&reference.relative_path);
    let mut candidate = canonical_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(CertificationError::UnsafeArtifactReference);
        };
        candidate.push(part);
        let metadata = fs::symlink_metadata(&candidate).map_err(|_| {
            CertificationError::ArtifactVerification("referenced artifact is unavailable")
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CertificationError::ArtifactVerification(
                "artifact path contains a symbolic link",
            ));
        }
    }

    let canonical = dunce::canonicalize(&candidate).map_err(|_| {
        CertificationError::ArtifactVerification("referenced artifact is unavailable")
    })?;
    if !canonical.starts_with(canonical_root) {
        return Err(CertificationError::ArtifactVerification(
            "artifact resolves outside fixture root",
        ));
    }
    let metadata = canonical.metadata().map_err(|_| {
        CertificationError::ArtifactVerification("referenced artifact is unavailable")
    })?;
    if !metadata.is_file() {
        return Err(CertificationError::ArtifactVerification(
            "referenced artifact is not a regular file",
        ));
    }
    if metadata.len() != reference.bytes {
        return Err(CertificationError::ArtifactVerification(
            "artifact byte count does not match evidence",
        ));
    }

    let read_limit = reference
        .bytes
        .checked_add(1)
        .ok_or(CertificationError::Bound("artifact read limit"))?;
    let file = fs::File::open(&canonical).map_err(|_| {
        CertificationError::ArtifactVerification("referenced artifact is unavailable")
    })?;
    let opened_metadata = file.metadata().map_err(|_| {
        CertificationError::ArtifactVerification("opened artifact metadata is unavailable")
    })?;
    if !opened_metadata.is_file() || !same_file(&metadata, &opened_metadata) {
        return Err(CertificationError::ArtifactVerification(
            "artifact changed while it was opened",
        ));
    }
    let mut bytes = Vec::with_capacity(reference.bytes.min(MAX_CAPTURE_BYTES as u64) as usize);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| CertificationError::ArtifactVerification("artifact could not be read"))?;
    if bytes.len() as u64 != reference.bytes {
        return Err(CertificationError::ArtifactVerification(
            "artifact changed while it was verified",
        ));
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != reference.sha256.to_ascii_lowercase() {
        return Err(CertificationError::ArtifactVerification(
            "artifact digest does not match evidence",
        ));
    }

    let text = std::str::from_utf8(&bytes).map_err(|_| {
        CertificationError::ArtifactVerification("opaque binary artifacts cannot be promoted")
    })?;
    scan_promotable_artifact(relative, text)
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

fn scan_promotable_artifact(relative_path: &Path, text: &str) -> Result<(), CertificationError> {
    scan_artifact_text_patterns(text)?;
    match relative_path.extension().and_then(|value| value.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("json") => {
            let value: Value = serde_json::from_str(text).map_err(|_| {
                CertificationError::ArtifactVerification("artifact is not valid JSON")
            })?;
            scan_value_for_forbidden_data(&value)
        }
        Some(extension) if extension.eq_ignore_ascii_case("sse") => scan_sse_artifact(text),
        _ => Err(CertificationError::ArtifactVerification(
            "only structured JSON and SSE artifacts can be promoted",
        )),
    }
}

fn scan_sse_artifact(text: &str) -> Result<(), CertificationError> {
    let mut data_lines = Vec::new();
    let mut observed_data = false;
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if data_lines.is_empty() {
                continue;
            }
            let data = data_lines.join("\n");
            data_lines.clear();
            observed_data = true;
            if data.trim() == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(&data).map_err(|_| {
                CertificationError::ArtifactVerification("SSE data is not valid JSON")
            })?;
            scan_value_for_forbidden_data(&value)?;
        } else if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if !observed_data {
        return Err(CertificationError::ArtifactVerification(
            "SSE artifact contains no data events",
        ));
    }
    Ok(())
}

fn scan_artifact_text_patterns(text: &str) -> Result<(), CertificationError> {
    let rule = forbidden_string_rule(text);
    if let Some(rule) = rule {
        return Err(CertificationError::ForbiddenData {
            path: "$artifact".into(),
            rule,
        });
    }

    for token in text.split(|character: char| {
        !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    }) {
        if looks_like_hostname(token) {
            return Err(CertificationError::ForbiddenData {
                path: "$artifact".into(),
                rule: "hostname-shaped value",
            });
        }
        if looks_like_high_entropy_token(token) {
            return Err(CertificationError::ForbiddenData {
                path: "$artifact".into(),
                rule: "high-entropy token-shaped value",
            });
        }
    }
    Ok(())
}

fn looks_like_hostname(token: &str) -> bool {
    let token = token.trim_matches('.');
    let Some(suffix) = token.rsplit('.').next() else {
        return false;
    };
    if !token.contains('.')
        || !matches!(
            suffix.to_ascii_lowercase().as_str(),
            "ai" | "com"
                | "net"
                | "org"
                | "io"
                | "dev"
                | "cloud"
                | "corp"
                | "internal"
                | "local"
                | "lan"
        )
    {
        return false;
    }
    token.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn looks_like_high_entropy_token(token: &str) -> bool {
    if token.len() < 40
        || token.bytes().all(|byte| byte.is_ascii_hexdigit())
        || (token.len() == 71
            && token.starts_with("opaque-")
            && token[7..].bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return false;
    }
    let has_alpha = token.bytes().any(|byte| byte.is_ascii_alphabetic());
    let digit_count = token.bytes().filter(u8::is_ascii_digit).count();
    let distinct = token.bytes().collect::<BTreeSet<_>>().len();
    has_alpha && digit_count >= 6 && distinct >= 12
}

fn validate_commit(value: &str) -> Result<(), CertificationError> {
    if !(7..=64).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CertificationError::Identifier("repository_commit"));
    }
    Ok(())
}

fn validate_optional_hash(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), CertificationError> {
    if let Some(value) = value {
        validate_hash(value, field)?;
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), CertificationError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CertificationError::Identifier(field));
    }
    Ok(())
}

fn validate_hash(value: &str, field: &'static str) -> Result<(), CertificationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CertificationError::Identifier(field));
    }
    Ok(())
}

fn validate_opaque_label(value: &str, field: &'static str) -> Result<(), CertificationError> {
    if value.len() != 71
        || !value.starts_with("opaque-")
        || !value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CertificationError::Identifier(field));
    }
    Ok(())
}

fn validate_optional_opaque_label(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), CertificationError> {
    if let Some(value) = value {
        validate_opaque_label(value, field)?;
    }
    Ok(())
}

fn validate_short_token(value: &str, field: &'static str) -> Result<(), CertificationError> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(CertificationError::Identifier(field));
    }
    Ok(())
}

fn validate_short_string(value: &str, field: &'static str) -> Result<(), CertificationError> {
    if value.is_empty() || value.len() > MAX_CAPTURE_STRING_BYTES || value.contains(['\r', '\n']) {
        return Err(CertificationError::Identifier(field));
    }
    Ok(())
}

fn validate_header_name(value: &str) -> Result<(), CertificationError> {
    if value.is_empty()
        || value.len() > 128
        || value != value.to_ascii_lowercase()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
    {
        return Err(CertificationError::Identifier("request header name"));
    }
    if is_secret_key(value) {
        return Err(CertificationError::ForbiddenData {
            path: "$.attempts[].presentRequestHeaders[]".into(),
            rule: "secret-bearing header names are not retained",
        });
    }
    Ok(())
}

fn validate_content_type(value: &str) -> Result<(), CertificationError> {
    validate_short_string(value, "response_content_type")?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'+' | b'-' | b'.' | b';' | b'=' | b' ')
    }) {
        return Err(CertificationError::Identifier("response_content_type"));
    }
    Ok(())
}

pub fn scan_value_for_forbidden_data(value: &Value) -> Result<(), CertificationError> {
    scan_value(value, "$", None)
}

fn scan_value(value: &Value, path: &str, key: Option<&str>) -> Result<(), CertificationError> {
    if let Some(key) = key {
        if is_secret_key(key) {
            return Err(CertificationError::ForbiddenData {
                path: path.into(),
                rule: "secret-bearing field name",
            });
        }
    }
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                scan_value(child, &format!("{path}.{name}"), Some(name))?;
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                scan_value(child, &format!("{path}[{index}]"), None)?;
            }
        }
        Value::String(text) => scan_string(text, path)?,
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn is_secret_key(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "xapikey"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "cookie"
            | "setcookie"
            | "password"
            | "secret"
            | "clientsecret"
            | "userid"
            | "username"
            | "teamid"
            | "organizationid"
            | "principalid"
            | "tenantid"
            | "accountid"
            | "subject"
            | "sub"
            | "email"
            | "machineid"
            | "deviceid"
    )
}

fn scan_string(text: &str, path: &str) -> Result<(), CertificationError> {
    if text.len() > MAX_CAPTURE_STRING_BYTES {
        return Err(CertificationError::ForbiddenData {
            path: path.into(),
            rule: "unbounded string",
        });
    }
    let rule = forbidden_string_rule(text);
    if let Some(rule) = rule {
        return Err(CertificationError::ForbiddenData {
            path: path.into(),
            rule,
        });
    }
    for token in text.split(|character: char| {
        !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    }) {
        if looks_like_high_entropy_token(token) {
            return Err(CertificationError::ForbiddenData {
                path: path.into(),
                rule: "high-entropy token-shaped value",
            });
        }
    }
    Ok(())
}

fn forbidden_string_rule(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("bearer ") {
        Some("bearer credential")
    } else if contains_prefixed_credential(&lower, "sk-")
        || contains_prefixed_credential(&lower, "xai-")
        || lower.contains("-----begin private key-----")
        || contains_jwt_shape(text)
    {
        Some("credential-shaped value")
    } else if lower.contains("/users/") || lower.contains("/home/") || lower.contains("c:\\users\\")
    {
        Some("host filesystem path")
    } else if lower.contains("http://") || lower.contains("https://") {
        Some("endpoint URL")
    } else {
        None
    }
}

fn contains_prefixed_credential(text: &str, prefix: &str) -> bool {
    let mut offset = 0usize;
    while let Some(relative) = text[offset..].find(prefix) {
        let start = offset + relative;
        let suffix = &text[start + prefix.len()..];
        let token_len = suffix
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            .count();
        let token = &suffix[..token_len];
        let digit_count = token.bytes().filter(u8::is_ascii_digit).count();
        let distinct = token.bytes().collect::<BTreeSet<_>>().len();
        let explicit_sk_prefix = prefix == "sk-" && start == 0 && token_len >= 8;
        let embedded_entropy =
            token_len >= 16 && distinct >= 10 && (digit_count >= 4 || token_len >= 24);
        if explicit_sk_prefix || embedded_entropy {
            return true;
        }
        offset = start + prefix.len();
    }
    false
}

fn contains_jwt_shape(text: &str) -> bool {
    text.split_ascii_whitespace().any(|candidate| {
        let candidate = candidate.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.')
        });
        let mut parts = candidate.split('.');
        let Some(header) = parts.next() else {
            return false;
        };
        let Some(payload) = parts.next() else {
            return false;
        };
        let Some(signature) = parts.next() else {
            return false;
        };
        parts.next().is_none()
            && candidate.len() >= 40
            && header.starts_with("eyJ")
            && [header, payload, signature].iter().all(|part| {
                part.len() >= 8
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn hash(ch: char) -> String {
        std::iter::repeat_n(ch, 64).collect()
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn set_artifact(reference: &mut ArtifactReference, bytes: &[u8]) {
        reference.sha256 = digest(bytes);
        reference.bytes = bytes.len() as u64;
    }

    fn materialize_fixture(capture: &mut PersistentAgentCapture) -> TempDir {
        let directory = tempfile::tempdir().unwrap();
        let attempt = directory.path().join("attempt-0001");
        fs::create_dir_all(&attempt).unwrap();
        let request =
            br#"{"model":"grok-code-fast-1","messages":[{"role":"user","content":"synthetic"}]}"#;
        let response = concat!(
            "data: {\"id\":\"synthetic\",\"object\":\"chat.completion.chunk\",",
            "\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"synthetic\",\"object\":\"chat.completion.chunk\",",
            "\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],",
            "\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n",
        )
        .as_bytes();
        fs::write(attempt.join("request.json"), request).unwrap();
        fs::write(attempt.join("response.sse"), response).unwrap();
        set_artifact(capture.attempts[0].request_body.as_mut().unwrap(), request);
        set_artifact(
            capture.attempts[0].response_body.as_mut().unwrap(),
            response,
        );
        capture.actuals.raw_artifact_bytes = (request.len() + response.len()) as u64;
        directory
    }

    fn fixture() -> PersistentAgentCapture {
        PersistentAgentCapture {
            schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
            campaign: CampaignIdentity {
                campaign_id: format!("opaque-{}", hash('9')),
                scenario_id: "resume-same-lane-001".into(),
                repository_commit: "b6dab133".into(),
                dirty: false,
            },
            provider: ProviderIdentity {
                route_class: ProviderRouteClass::GrokBuildProxy,
                dialect: ProviderDialectClass::XaiChatCompletions,
                credential_method: CredentialMethodClass::GrokBuildOidc,
                model_identity: "grok-code-fast-1".into(),
                endpoint_fingerprint: public_xai_endpoint_fingerprint(
                    &ProviderRouteClass::GrokBuildProxy,
                )
                .unwrap(),
            },
            budgets: CampaignBudgets {
                bound_profile: CertificationBoundProfile::Standard,
                max_run_tokens: 100_000,
                max_total_tokens: 100_000,
                max_provider_requests: 64,
                max_continuations: 16,
                max_duration_seconds: 7_200,
                max_raw_artifact_bytes: 64 * 1024 * 1024,
                max_response_bytes_per_request: 8 * 1024 * 1024,
            },
            actuals: CampaignActuals {
                total_tokens: 15,
                provider_requests: 1,
                continuations: 1,
                duration_seconds: 1,
                raw_artifact_bytes: 777,
            },
            attempts: vec![ProviderAttemptEvidence {
                attempt: 1,
                method: "POST".into(),
                route_identity: "/v1/chat/completions".into(),
                present_request_headers: vec!["content-type".into(), "user-agent".into()],
                request_body: Some(ArtifactReference {
                    relative_path: "attempt-0001/request.json".into(),
                    sha256: hash('b'),
                    bytes: 321,
                }),
                response_body: Some(ArtifactReference {
                    relative_path: "attempt-0001/response.sse".into(),
                    sha256: hash('c'),
                    bytes: 456,
                }),
                response_status: Some(200),
                response_content_type: Some("text/event-stream".into()),
                framing: StreamFraming::Sse,
                disposition: AttemptDisposition::Success,
                usage: Some(UsageEvidence {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    complete: true,
                }),
                latency_millis: 25,
            }],
            durable_states: vec![DurableStateEvidence {
                role: DurableEvidenceRole::Primary,
                agent_id: format!("opaque-{}", hash('1')),
                agent_spec_revision: 2,
                lane_id: format!("opaque-{}", hash('2')),
                run_id: format!("opaque-{}", hash('3')),
                parent_run_id: Some(format!("opaque-{}", hash('4'))),
                checkpoint_id: Some(format!("opaque-{}", hash('5'))),
                continuation_input_hash: Some(hash('d')),
                continuation_context_hash: Some(hash('e')),
                continuation_fidelity: Some("complete".into()),
                terminal_state: "completed".into(),
                stop_cause: None,
            }],
            checks: vec![
                CertificationCheck {
                    name: "new_finite_run_created".into(),
                    passed: true,
                    detail_code: "run-created".into(),
                },
                CertificationCheck {
                    name: "verified_checkpoint_used".into(),
                    passed: true,
                    detail_code: "parent-and-context-match".into(),
                },
                CertificationCheck {
                    name: "provider_observation_complete".into(),
                    passed: true,
                    detail_code: "recorder-complete".into(),
                },
                CertificationCheck {
                    name: "provider_attempts_bound_to_durable_run".into(),
                    passed: true,
                    detail_code: "run-scope-match".into(),
                },
            ],
        }
    }

    #[test]
    fn public_xai_fixture_passes_the_capture_and_promotion_gates() {
        let mut fixture = fixture();
        let directory = materialize_fixture(&mut fixture);
        fixture.validate().unwrap();
        fixture
            .validate_for_xai_fixture_promotion_at(directory.path())
            .unwrap();
    }

    #[test]
    fn legacy_single_run_capture_defaults_to_primary_evidence_role() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["durableStates"][0]
            .as_object_mut()
            .unwrap()
            .remove("role");
        let capture: PersistentAgentCapture = serde_json::from_value(value).unwrap();
        assert_eq!(capture.durable_states[0].role, DurableEvidenceRole::Primary);
        capture.validate_complete_structural_evidence().unwrap();
    }

    #[test]
    fn private_gateway_requires_opaque_identity_and_is_never_promotable() {
        let mut capture = fixture();
        capture.provider.route_class = ProviderRouteClass::CompatibleGateway;
        capture.provider.dialect = ProviderDialectClass::OpenAiCompatible;
        capture.provider.credential_method = CredentialMethodClass::ManagedProviderReference;
        capture.provider.model_identity = format!("opaque-{}", hash('1'));
        capture.attempts[0].route_identity = format!("opaque-{}", hash('2'));
        capture.validate().unwrap();
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(Path::new(".")),
            Err(CertificationError::NotPromotable(_))
        ));
    }

    #[test]
    fn scanner_reports_location_and_rule_without_echoing_secret() {
        let secret = "Bearer do-not-repeat-this-value";
        let error = scan_value_for_forbidden_data(&json!({"nested": {"value": secret}}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("$.nested.value"));
        assert!(error.contains("bearer credential"));
        assert!(!error.contains(secret));
    }

    #[test]
    fn scanner_rejects_secret_fields_paths_and_endpoints() {
        for value in [
            json!({"authorization": "redacted"}),
            json!({"value": "/Users/alice/private/repo"}),
            json!({"value": "https://private.example/path"}),
            json!({"value": "sk-test-value"}),
            json!({"value": "prefix-xai-0123456789abcdef-suffix"}),
            json!({"value": "prefix-sk-0123456789abcdef-suffix"}),
            json!({"value": "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJwcml2YXRlLXVzZXIifQ.signature_material_123456"}),
            json!({"value": "tokenMaterialWithManyDistinctChars0123456789abcdefghijk"}),
        ] {
            assert!(matches!(
                scan_value_for_forbidden_data(&value),
                Err(CertificationError::ForbiddenData { .. })
            ));
        }

        for public_identifier in [
            "xai-route-oidc-001",
            "x-xai-token-auth",
            "native-xai-grok-build-oidc",
        ] {
            scan_value_for_forbidden_data(&json!({"value": public_identifier})).unwrap();
        }
    }

    #[test]
    fn scanner_rejects_provider_identity_and_account_fields() {
        for field in [
            "user_id",
            "teamId",
            "organization-id",
            "principal_id",
            "tenantId",
            "account_id",
            "subject",
            "sub",
            "email",
            "machine_id",
            "deviceId",
        ] {
            assert!(matches!(
                scan_value_for_forbidden_data(&json!({(field): "short-value"})),
                Err(CertificationError::ForbiddenData { .. })
            ));
        }
    }

    #[test]
    fn artifact_paths_are_relative_and_cannot_escape() {
        let mut capture = fixture();
        for unsafe_path in ["../response.sse", "/tmp/response.sse"] {
            capture.attempts[0]
                .response_body
                .as_mut()
                .unwrap()
                .relative_path = unsafe_path.into();
            assert_eq!(
                capture.validate().unwrap_err(),
                CertificationError::UnsafeArtifactReference
            );
        }
    }

    #[test]
    fn secret_header_names_are_not_retained_even_without_values() {
        let mut capture = fixture();
        capture.attempts[0]
            .present_request_headers
            .push("authorization".into());
        assert!(matches!(
            capture.validate(),
            Err(CertificationError::ForbiddenData { .. })
        ));
    }

    #[test]
    fn usage_and_attempt_sequence_fail_closed() {
        let mut capture = fixture();
        capture.attempts[0].attempt = 2;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("attempt sequence")
        );

        let mut capture = fixture();
        capture.attempts[0].usage.as_mut().unwrap().total_tokens = 14;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("complete token usage")
        );

        let mut capture = fixture();
        capture.attempts[0].disposition = AttemptDisposition::Retried;
        capture.attempts[0].response_status = Some(503);
        capture.attempts[0].response_content_type = Some("application/json".into());
        capture.attempts[0].framing = StreamFraming::Json;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("retried attempt has no following attempt")
        );
    }

    #[test]
    fn campaign_identity_hash_and_lowercase_repository_state_are_required() {
        let mut capture = fixture();
        capture.schema = "grokptah.persistent_agent_capture.v1".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::UnsupportedSchema
        );

        let mut capture = fixture();
        capture.campaign.campaign_id = "raw-campaign-identifier".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("campaign_id")
        );

        let mut capture = fixture();
        capture.campaign.repository_commit = "ABCDEF12".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("repository_commit")
        );

        let mut capture = fixture();
        capture.durable_states[0].run_id = "run-actual-must-not-escape".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("run_id")
        );
    }

    #[test]
    fn promotion_requires_passing_checks_authoritative_usage_and_successful_causality() {
        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.checks.clear();
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.checks[0].passed = false;
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.attempts[0].usage.as_mut().unwrap().complete = false;
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.checks.pop();
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.durable_states.clear();
        capture.actuals.continuations = 0;
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.attempts[0].disposition = AttemptDisposition::ProviderRejected;
        capture.attempts[0].response_status = Some(500);
        capture.attempts[0].response_content_type = Some("application/json".into());
        capture.attempts[0].framing = StreamFraming::Json;
        capture.durable_states[0].terminal_state = "failed".into();
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));
    }

    #[test]
    fn dirty_campaign_is_retained_as_evidence_but_not_promoted() {
        let mut capture = fixture();
        capture.campaign.dirty = true;
        capture.validate().unwrap();
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(Path::new(".")),
            Err(CertificationError::NotPromotable(_))
        ));
    }

    #[test]
    fn public_xai_promotion_requires_the_exact_allowlisted_wire_contract() {
        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.provider.endpoint_fingerprint = hash('f');
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        capture.provider.dialect = ProviderDialectClass::OpenAiCompatible;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier(
                "public xAI route requires xAI chat-completions dialect"
            )
        );

        let mut capture = fixture();
        capture.attempts[0].route_identity = "/private/chat/completions".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("public xAI request route")
        );

        let mut capture = fixture();
        capture.attempts[0].method = "GET".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("public xAI request route")
        );
    }

    #[test]
    fn aggregate_actuals_must_match_evidence_and_budgets() {
        let mut capture = fixture();
        capture.actuals.provider_requests = 2;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("provider request actual does not match attempts")
        );

        let mut capture = fixture();
        capture.actuals.total_tokens = 16;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("token actual does not match attempt usage")
        );

        let mut capture = fixture();
        capture.budgets.max_total_tokens = 14;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Bound("campaign budgets exceed bound profile")
        );

        let mut capture = fixture();
        capture.actuals.duration_seconds = capture.budgets.max_duration_seconds + 1;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Bound("campaign duration actual")
        );

        let mut capture = fixture();
        capture.actuals.raw_artifact_bytes = 776;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("artifact actual does not match referenced artifacts")
        );

        let mut capture = fixture();
        let mut terminal = capture.attempts[0].clone();
        capture.attempts[0].disposition = AttemptDisposition::Retried;
        capture.attempts[0].response_status = Some(503);
        capture.attempts[0].response_content_type = Some("application/json".into());
        capture.attempts[0].framing = StreamFraming::Json;
        capture.attempts[0].usage = Some(UsageEvidence {
            prompt_tokens: 4,
            completion_tokens: 4,
            total_tokens: 8,
            complete: true,
        });
        terminal.attempt = 2;
        terminal.usage = Some(UsageEvidence {
            prompt_tokens: 4,
            completion_tokens: 3,
            total_tokens: 7,
            complete: true,
        });
        capture.attempts.push(terminal);
        capture.actuals.provider_requests = 2;
        capture.actuals.total_tokens = 15;
        capture.budgets.max_run_tokens = 10;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Bound("per-Run aggregate token usage")
        );
    }

    #[test]
    fn conflicting_artifact_evidence_fails_closed() {
        let mut capture = fixture();
        let response = capture.attempts[0].response_body.as_mut().unwrap();
        response.relative_path = "attempt-0001/request.json".into();
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("artifact path has conflicting evidence")
        );
    }

    #[test]
    fn promotion_requires_a_catalog_scenario_and_verified_artifacts() {
        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.campaign.scenario_id = "unregistered-scenario".into();
        assert!(matches!(
            capture.validate_for_xai_fixture_promotion_at(directory.path()),
            Err(CertificationError::NotPromotable(_))
        ));

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.attempts[0].response_body.as_mut().unwrap().sha256 = hash('f');
        assert_eq!(
            capture
                .validate_for_xai_fixture_promotion_at(directory.path())
                .unwrap_err(),
            CertificationError::ArtifactVerification("artifact digest does not match evidence")
        );

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        capture.attempts[0].response_body.as_mut().unwrap().bytes += 1;
        capture.actuals.raw_artifact_bytes += 1;
        assert_eq!(
            capture
                .validate_for_xai_fixture_promotion_at(directory.path())
                .unwrap_err(),
            CertificationError::ArtifactVerification("artifact byte count does not match evidence")
        );
    }

    #[test]
    fn promotion_scans_structured_artifact_content() {
        for unsafe_request in [
            br#"{"authorization":"redacted"}"#.as_slice(),
            br#"{"host":"gateway.private-corp.com"}"#.as_slice(),
            br#"{"value":"tokenMaterialWithManyDistinctChars0123456789abcdefghijk"}"#.as_slice(),
        ] {
            let mut capture = fixture();
            let directory = materialize_fixture(&mut capture);
            fs::write(
                directory.path().join("attempt-0001/request.json"),
                unsafe_request,
            )
            .unwrap();
            let old_bytes = capture.attempts[0].request_body.as_ref().unwrap().bytes;
            set_artifact(
                capture.attempts[0].request_body.as_mut().unwrap(),
                unsafe_request,
            );
            capture.actuals.raw_artifact_bytes = capture
                .actuals
                .raw_artifact_bytes
                .saturating_sub(old_bytes)
                .saturating_add(unsafe_request.len() as u64);
            assert!(matches!(
                capture.validate_for_xai_fixture_promotion_at(directory.path()),
                Err(CertificationError::ForbiddenData { .. })
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rejects_symlinked_artifacts() {
        use std::os::unix::fs::symlink;

        let mut capture = fixture();
        let directory = materialize_fixture(&mut capture);
        let response_path = directory.path().join("attempt-0001/response.sse");
        fs::remove_file(&response_path).unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        symlink(external.path(), response_path).unwrap();
        assert_eq!(
            capture
                .validate_for_xai_fixture_promotion_at(directory.path())
                .unwrap_err(),
            CertificationError::ArtifactVerification("artifact path contains a symbolic link")
        );
    }

    #[test]
    fn versioned_scenario_catalog_is_valid_unique_and_within_hard_bounds() {
        let catalog: Value = serde_json::from_str(include_str!(
            "../../../../evals/persistent-agent-scenarios.v1.json"
        ))
        .unwrap();
        assert_eq!(catalog["schema"], "grokptah.persistent_agent_scenarios.v1");

        let scenarios = catalog["scenarios"].as_array().unwrap();
        let ids: BTreeSet<&str> = scenarios
            .iter()
            .map(|scenario| scenario["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), scenarios.len(), "scenario IDs must be unique");
        let compiled_ids: BTreeSet<&str> = PERSISTENT_AGENT_SCENARIO_IDS.iter().copied().collect();
        assert_eq!(
            ids, compiled_ids,
            "compiled promotion allowlist must match the versioned catalog"
        );
        for required in [
            "xai-route-oidc-001",
            "sse-stream-001",
            "native-tools-001",
            "restart-between-runs-001",
            "resume-cross-lane-001",
            "archive-lane-001",
            "memory-scopes-001",
            "token-ceiling-001",
            "endurance-finite-runs-001",
        ] {
            assert!(ids.contains(required), "missing scenario {required}");
        }

        for profile in catalog["bound_profiles"].as_object().unwrap().values() {
            assert!(
                profile["raw_artifact_byte_ceiling"].as_u64().unwrap() <= MAX_RAW_ARTIFACT_BYTES
            );
            assert!(
                profile["provider_request_ceiling"].as_u64().unwrap()
                    <= MAX_CAPTURE_ATTEMPTS as u64
            );
        }
        for (name, typed) in [
            ("smoke", CertificationBoundProfile::Smoke),
            ("standard", CertificationBoundProfile::Standard),
            ("extended", CertificationBoundProfile::Extended),
        ] {
            let profile = &catalog["bound_profiles"][name];
            let limits = typed.limits();
            assert_eq!(profile["per_run_token_ceiling"], limits.per_run_tokens);
            assert_eq!(profile["campaign_token_ceiling"], limits.campaign_tokens);
            assert_eq!(
                profile["provider_request_ceiling"],
                limits.provider_requests
            );
            assert_eq!(profile["continuation_ceiling"], limits.continuations);
            assert_eq!(profile["duration_seconds"], limits.duration_seconds);
            assert_eq!(profile["raw_artifact_byte_ceiling"], limits.artifact_bytes);
            assert_eq!(
                profile["response_byte_ceiling_per_request"],
                limits.response_bytes
            );
            assert_eq!(profile["missing_usage_policy"], "fail_closed");
        }
    }
}
