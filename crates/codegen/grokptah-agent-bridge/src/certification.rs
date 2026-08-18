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

pub const PERSISTENT_AGENT_CAPTURE_SCHEMA: &str = "grokptah.persistent_agent_capture.v1";
pub const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CAPTURE_ATTEMPTS: usize = 4_096;
pub const MAX_CAPTURE_CHECKS: usize = 2_048;
pub const MAX_CAPTURE_STRING_BYTES: usize = 8 * 1024;
pub const MAX_RAW_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
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
    "memory-scopes-001",
    "spec-revision-001",
    "token-ceiling-001",
    "endurance-finite-runs-001",
];
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CampaignIdentity {
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
                .response_body
                .as_ref()
                .is_some_and(|body| body.bytes > self.budgets.max_response_bytes_per_request)
            {
                return Err(CertificationError::Bound(
                    "response artifact exceeds per-request budget",
                ));
            }
        }
        for state in &self.durable_states {
            validate_identifier(&state.agent_id, "agent_id")?;
            validate_identifier(&state.lane_id, "lane_id")?;
            validate_identifier(&state.run_id, "run_id")?;
            validate_optional_identifier(state.parent_run_id.as_deref(), "parent_run_id")?;
            validate_optional_identifier(state.checkpoint_id.as_deref(), "checkpoint_id")?;
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
        for check in &self.checks {
            validate_identifier(&check.name, "check name")?;
            validate_short_token(&check.detail_code, "check detail_code")?;
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
        verify_promotion_artifacts(self, fixture_root)?;
        Ok(())
    }
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
    if budgets.max_total_tokens == 0
        || budgets.max_provider_requests == 0
        || budgets.max_continuations == 0
        || budgets.max_duration_seconds == 0
        || budgets.max_raw_artifact_bytes == 0
        || budgets.max_response_bytes_per_request == 0
    {
        return Err(CertificationError::Bound("zero campaign budget"));
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
        if usage.complete && usage.total_tokens != summed {
            return Err(CertificationError::Identifier("complete token usage"));
        }
    }
    Ok(())
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
    let lower = text.to_ascii_lowercase();
    let rule = if lower.contains("bearer ") {
        Some("bearer credential")
    } else if lower.contains("sk-")
        || lower.contains("xai-")
        || lower.contains("-----begin private key-----")
    {
        Some("credential-shaped value")
    } else if lower.contains("/users/") || lower.contains("/home/") || lower.contains("c:\\users\\")
    {
        Some("host filesystem path")
    } else if lower.contains("http://") || lower.contains("https://") {
        Some("endpoint URL")
    } else {
        None
    };
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
    if token.len() < 40 || token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let has_alpha = token.bytes().any(|byte| byte.is_ascii_alphabetic());
    let has_digit = token.bytes().any(|byte| byte.is_ascii_digit());
    let distinct = token.bytes().collect::<BTreeSet<_>>().len();
    has_alpha && has_digit && distinct >= 12
}

fn validate_commit(value: &str) -> Result<(), CertificationError> {
    if !(7..=64).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CertificationError::Identifier("repository_commit"));
    }
    Ok(())
}

fn validate_optional_identifier(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), CertificationError> {
    if let Some(value) = value {
        validate_identifier(value, field)?;
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
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CertificationError::Identifier(field));
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
    let lower = text.to_ascii_lowercase();
    let rule = if lower.contains("bearer ") {
        Some("bearer credential")
    } else if lower.starts_with("sk-") || lower.contains("-----begin private key-----") {
        Some("credential-shaped value")
    } else if lower.contains("/users/") || lower.contains("/home/") || lower.contains("c:\\users\\")
    {
        Some("host filesystem path")
    } else if lower.contains("http://") || lower.contains("https://") {
        Some("endpoint URL")
    } else {
        None
    };
    if let Some(rule) = rule {
        return Err(CertificationError::ForbiddenData {
            path: path.into(),
            rule,
        });
    }
    Ok(())
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
                agent_id: "agent-fixture".into(),
                agent_spec_revision: 2,
                lane_id: "lane-fixture".into(),
                run_id: "run-fixture".into(),
                parent_run_id: Some("run-parent".into()),
                checkpoint_id: Some("checkpoint-fixture".into()),
                continuation_input_hash: Some(hash('d')),
                continuation_context_hash: Some(hash('e')),
                continuation_fidelity: Some("complete".into()),
                terminal_state: "completed".into(),
                stop_cause: None,
            }],
            checks: vec![CertificationCheck {
                name: "checkpoint-linked".into(),
                passed: true,
                detail_code: "parent-and-context-match".into(),
            }],
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
        ] {
            assert!(matches!(
                scan_value_for_forbidden_data(&value),
                Err(CertificationError::ForbiddenData { .. })
            ));
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
        capture.attempts[0].usage.as_mut().unwrap().total_tokens = 16;
        assert_eq!(
            capture.validate().unwrap_err(),
            CertificationError::Identifier("complete token usage")
        );
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
            CertificationError::Bound("aggregate token usage")
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
    }
}
