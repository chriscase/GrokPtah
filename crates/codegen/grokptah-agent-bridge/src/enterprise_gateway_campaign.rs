//! Deterministic enterprise-gateway campaign evidence.
//!
//! This module records restricted-company gateway identity, quota receipts,
//! retry attempts, Cursor-account presence, and release/promotion gates. It
//! never contacts a live company gateway, invents credentials, or treats an
//! offline fixture as live proof. A passing fixture makes the live-evidence
//! requirements explicit; it does not qualify a release.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use grokptah_agent_sdk::run::MAX_REQUEST_ID_BYTES;
use grokptah_agent_sdk::{ErrorCode, ErrorEnvelope, ExternalWorkerProvider};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gateway_config::{
    normalized_profile_id, validate_base_url, ProviderKind, XAI_PROVIDER_ID,
};

/// Contract identifier for enterprise-gateway campaign bundles.
pub const ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA: &str = "grokptah.enterprise-gateway-campaign.v1";

/// How an evidence field was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// No receipt exists. Release and quota gates fail closed.
    Absent,
    /// Loopback or scripted provider. Never live proof.
    OfflineFixture,
    /// Named live campaign against a non-loopback company/Cursor route.
    LiveCampaign,
}

/// Provider class selected for an enterprise review route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayClass {
    /// Fixed company/restricted model route.
    RestrictedCompany,
    /// Built-in frontier family. Must not be a silent fallback.
    Frontier,
    /// Identity was not recorded.
    Unknown,
}

/// Recorded gateway identity for one campaign request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayIdentityRecord {
    /// Local profile ID from [`crate::gateway_config::ProviderProfile`].
    pub profile_id: String,
    /// Provider base URL. Loopback HTTP is fixture-only.
    pub base_url: String,
    /// Exact model ID bound to the profile.
    pub model_id: String,
    /// Optional tenant/authorization boundary label. Never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Restricted vs frontier class.
    pub class: GatewayClass,
    /// Provider family. Reuses the gateway-config authority boundary.
    pub provider_kind: ProviderKind,
}

/// Quota/usage truth for one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuotaTruth {
    /// Values taken from an explicit provider receipt.
    #[serde(rename_all = "camelCase")]
    ProviderReceipt {
        used: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
        unit: String,
        /// Must be `provider`. Local inference is not quota truth.
        source: String,
        evidence_kind: EvidenceKind,
    },
    /// No provider receipt. Fail closed.
    Unknown,
    /// Two receipts or arithmetic that cannot both be true. Fail closed.
    #[serde(rename_all = "camelCase")]
    Contradictory { detail: String },
}

/// One provider quota receipt bound to a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaReceipt {
    pub request_id: String,
    pub truth: QuotaTruth,
}

/// Outcome of one auditable attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    Failed,
    Replayed,
}

/// Request/attempt/outcome receipt for idempotent retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptReceipt {
    pub request_id: String,
    pub attempt: u32,
    pub payload_hash: String,
    pub outcome: AttemptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
}

/// Cursor-account evidence. Credentials never belong here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorAccountEvidence {
    pub provider: ExternalWorkerProvider,
    pub kind: EvidenceKind,
}

/// Release/promotion evidence kinds. Live fields must not be inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleasePromotionEvidence {
    pub live_gateway: EvidenceKind,
    pub live_quota: EvidenceKind,
    pub live_cursor_account: EvidenceKind,
}

/// One campaign bundle evaluated by [`verify_campaign`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignBundle {
    pub schema: String,
    pub requested: GatewayIdentityRecord,
    pub observed: GatewayIdentityRecord,
    pub quota: QuotaReceipt,
    pub attempts: Vec<AttemptReceipt>,
    pub cursor_account: CursorAccountEvidence,
    pub promotion: ReleasePromotionEvidence,
}

/// Named check recorded on a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl CampaignCheck {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            passed: true,
            detail: detail.into(),
        }
    }

    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            passed: false,
            detail: detail.into(),
        }
    }
}

/// Verifier output. Offline fixtures can pass contract checks and still refuse
/// release qualification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignVerdict {
    pub schema: String,
    /// Identity, quota fail-closed, retry, redaction, and promotion-gate checks.
    pub contract_passed: bool,
    /// True only when live gateway, quota, and Cursor-account evidence are
    /// present and contract checks passed. Offline fixtures never set this.
    pub qualified_for_release: bool,
    pub checks: Vec<CampaignCheck>,
    pub remaining_live_gates: Vec<String>,
}

/// In-process fake restricted gateway used by offline tests.
#[derive(Debug)]
pub struct FakeRestrictedGateway {
    identity: GatewayIdentityRecord,
    quota_mode: FakeQuotaMode,
    unavailable: bool,
    leak_secret: Option<String>,
    attempts: Mutex<HashMap<String, StoredAttempt>>,
}

/// Quota behavior for [`FakeRestrictedGateway`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeQuotaMode {
    /// Explicit provider receipt.
    ProviderReceipt,
    /// Omit usage. Verifier must fail closed.
    Unknown,
    /// Arithmetic that cannot be true.
    Contradictory,
    /// Local session counters presented as usage. Not a provider receipt.
    LocalInference,
}

#[derive(Debug, Clone)]
struct StoredAttempt {
    payload_hash: String,
    quota: QuotaReceipt,
    error: Option<ErrorEnvelope>,
    count: u32,
}

impl FakeRestrictedGateway {
    /// Restricted company fixture. Loopback URLs stay [`EvidenceKind::OfflineFixture`].
    pub fn restricted_loopback(base_url: impl Into<String>) -> Self {
        Self {
            identity: GatewayIdentityRecord {
                profile_id: "corp-restricted".into(),
                base_url: base_url.into(),
                model_id: "company-code-small".into(),
                tenant: Some("acme-tenant".into()),
                class: GatewayClass::RestrictedCompany,
                provider_kind: ProviderKind::OpenAiCompatible,
            },
            quota_mode: FakeQuotaMode::ProviderReceipt,
            unavailable: false,
            leak_secret: None,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_silent_frontier_fallback(mut self) -> Self {
        self.identity.class = GatewayClass::Frontier;
        self.identity.provider_kind = ProviderKind::Xai;
        self.identity.profile_id = XAI_PROVIDER_ID.into();
        self.identity.model_id = "grok-4.5".into();
        self.identity.base_url = "https://api.x.ai/v1".into();
        self
    }

    #[must_use]
    pub fn with_quota_mode(mut self, quota_mode: FakeQuotaMode) -> Self {
        self.quota_mode = quota_mode;
        self
    }

    #[must_use]
    pub fn with_unavailable(mut self) -> Self {
        self.unavailable = true;
        self
    }

    #[must_use]
    pub fn with_leaked_secret(mut self, secret: impl Into<String>) -> Self {
        self.leak_secret = Some(secret.into());
        self
    }

    pub fn requested_identity(&self) -> GatewayIdentityRecord {
        GatewayIdentityRecord {
            profile_id: "corp-restricted".into(),
            base_url: self.identity.base_url.clone(),
            model_id: "company-code-small".into(),
            tenant: Some("acme-tenant".into()),
            class: GatewayClass::RestrictedCompany,
            provider_kind: ProviderKind::OpenAiCompatible,
        }
    }

    /// Probe the fake gateway. Identical retries replay; payload drift fails.
    pub fn probe(
        &self,
        request_id: &str,
        payload: &str,
    ) -> Result<(GatewayIdentityRecord, QuotaReceipt, AttemptReceipt), ErrorEnvelope> {
        validate_request_id(request_id).map_err(|message| {
            bounded_provider_error(ErrorCode::InvalidRequest, message, request_id, None)
        })?;
        let payload_hash = campaign_payload_hash(request_id, payload);
        let mut attempts = self.attempts.lock().expect("fake gateway lock");
        if let Some(prior) = attempts.get_mut(request_id) {
            prior.count = prior.count.saturating_add(1);
            if prior.payload_hash != payload_hash {
                let error = bounded_provider_error(
                    ErrorCode::InvalidRequest,
                    "request_id reused with a different payload",
                    request_id,
                    self.leak_secret.as_deref(),
                );
                return Err(error);
            }
            let attempt = AttemptReceipt {
                request_id: request_id.into(),
                attempt: prior.count,
                payload_hash: payload_hash.clone(),
                outcome: AttemptOutcome::Replayed,
                error: prior.error.clone(),
            };
            if let Some(error) = prior.error.clone() {
                return Err(error);
            }
            return Ok((self.identity.clone(), prior.quota.clone(), attempt));
        }

        if self.unavailable {
            let raw = match self.leak_secret.as_deref() {
                Some(secret) => format!(
                    "upstream 503 api_key={secret} Authorization: Bearer {secret} url=https://evil.example/v1"
                ),
                None => "The requested provider is unavailable.".into(),
            };
            let error = bounded_provider_error(
                ErrorCode::AuthorityUnavailable,
                &raw,
                request_id,
                self.leak_secret.as_deref(),
            );
            attempts.insert(
                request_id.into(),
                StoredAttempt {
                    payload_hash: payload_hash.clone(),
                    quota: QuotaReceipt {
                        request_id: request_id.into(),
                        truth: QuotaTruth::Unknown,
                    },
                    error: Some(error.clone()),
                    count: 1,
                },
            );
            return Err(error);
        }

        let quota = QuotaReceipt {
            request_id: request_id.into(),
            truth: match self.quota_mode {
                FakeQuotaMode::ProviderReceipt => QuotaTruth::ProviderReceipt {
                    used: 12,
                    remaining: Some(88),
                    limit: Some(100),
                    unit: "requests".into(),
                    source: "provider".into(),
                    evidence_kind: EvidenceKind::OfflineFixture,
                },
                FakeQuotaMode::Unknown => QuotaTruth::Unknown,
                FakeQuotaMode::Contradictory => QuotaTruth::ProviderReceipt {
                    used: 40,
                    remaining: Some(80),
                    limit: Some(100),
                    unit: "requests".into(),
                    source: "provider".into(),
                    evidence_kind: EvidenceKind::OfflineFixture,
                },
                FakeQuotaMode::LocalInference => QuotaTruth::ProviderReceipt {
                    used: 12,
                    remaining: None,
                    limit: None,
                    unit: "tokens".into(),
                    source: "local_session".into(),
                    evidence_kind: EvidenceKind::OfflineFixture,
                },
            },
        };
        let attempt = AttemptReceipt {
            request_id: request_id.into(),
            attempt: 1,
            payload_hash: payload_hash.clone(),
            outcome: AttemptOutcome::Succeeded,
            error: None,
        };
        attempts.insert(
            request_id.into(),
            StoredAttempt {
                payload_hash,
                quota: quota.clone(),
                error: None,
                count: 1,
            },
        );
        Ok((self.identity.clone(), quota, attempt))
    }

    /// Build an offline campaign bundle, including an identical retry.
    pub fn collect_offline_campaign(
        &self,
        request_id: &str,
        payload: &str,
    ) -> Result<CampaignBundle, ErrorEnvelope> {
        let requested = self.requested_identity();
        let (observed, quota, first) = self.probe(request_id, payload)?;
        let retry = match self.probe(request_id, payload) {
            Ok((_, _, retry)) => retry,
            Err(error) => AttemptReceipt {
                request_id: request_id.into(),
                attempt: 2,
                payload_hash: campaign_payload_hash(request_id, payload),
                outcome: AttemptOutcome::Replayed,
                error: Some(error),
            },
        };
        Ok(CampaignBundle {
            schema: ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA.into(),
            requested,
            observed,
            quota,
            attempts: vec![first, retry],
            cursor_account: CursorAccountEvidence {
                provider: ExternalWorkerProvider::CursorCloud,
                kind: EvidenceKind::Absent,
            },
            promotion: ReleasePromotionEvidence {
                live_gateway: EvidenceKind::Absent,
                live_quota: EvidenceKind::Absent,
                live_cursor_account: EvidenceKind::Absent,
            },
        })
    }
}

/// SHA-256 identity for one campaign payload. Matches ledger-style hex.
pub fn campaign_payload_hash(request_id: &str, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request_id.as_bytes());
    hasher.update([0xff]);
    hasher.update(payload.as_bytes());
    hex_sha256(&hasher.finalize())
}

/// Share-safe provider error. Raw bodies and secrets never cross this boundary.
pub fn bounded_provider_error(
    code: ErrorCode,
    message: &str,
    request_id: &str,
    leaked_secret: Option<&str>,
) -> ErrorEnvelope {
    let mut message = redact_public_text(message);
    if let Some(secret) = leaked_secret.filter(|value| !value.is_empty()) {
        message = message.replace(secret, "[redacted]");
    }
    if message.trim().is_empty() {
        message = "The provider request failed.".into();
    }
    let reason_code = match code {
        ErrorCode::AuthorityUnavailable => Some("provider_unavailable".into()),
        ErrorCode::Capacity => Some("rate_limited".into()),
        ErrorCode::InvalidRequest => Some("invalid_request".into()),
        ErrorCode::ForbiddenScope => Some("forbidden_scope".into()),
        _ => Some("provider_error".into()),
    };
    ErrorEnvelope {
        code,
        message,
        request_id: Some(request_id.into()),
        reason_code,
        event_range: None,
    }
}

/// Evaluate a campaign bundle. Does not perform network I/O.
pub fn verify_campaign(bundle: &CampaignBundle) -> CampaignVerdict {
    let checks = vec![
        schema_check(bundle),
        identity_recorded_check(bundle),
        no_silent_frontier_fallback_check(bundle),
        quota_receipt_check(bundle),
        retry_idempotency_check(bundle),
        redaction_least_privilege_check(bundle),
        promotion_refuses_absent_live_check(bundle),
    ];

    let contract_passed = checks.iter().all(|check| check.passed);
    let remaining_live_gates = remaining_live_gates(bundle);
    let qualified_for_release = contract_passed
        && remaining_live_gates.is_empty()
        && bundle.requested.class == GatewayClass::RestrictedCompany
        && !is_frontier(&bundle.observed)
        && url_can_be_live(&bundle.observed.base_url);

    CampaignVerdict {
        schema: ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA.into(),
        contract_passed,
        qualified_for_release,
        checks,
        remaining_live_gates,
    }
}

fn schema_check(bundle: &CampaignBundle) -> CampaignCheck {
    if bundle.schema == ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA {
        CampaignCheck::pass(
            "schema",
            format!("bundle uses {ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA}"),
        )
    } else {
        CampaignCheck::fail(
            "schema",
            format!(
                "expected {ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA}, got {}",
                bundle.schema
            ),
        )
    }
}

fn identity_recorded_check(bundle: &CampaignBundle) -> CampaignCheck {
    let requested = validate_identity_record(&bundle.requested);
    let observed = validate_identity_record(&bundle.observed);
    match (requested, observed) {
        (Ok(()), Ok(())) if bundle.observed.class != GatewayClass::Unknown => CampaignCheck::pass(
            "restricted_gateway_identity",
            format!(
                "recorded profile `{}` model `{}` class {:?}",
                bundle.observed.profile_id, bundle.observed.model_id, bundle.observed.class
            ),
        ),
        (Err(detail), _) | (_, Err(detail)) => {
            CampaignCheck::fail("restricted_gateway_identity", detail)
        }
        _ => CampaignCheck::fail(
            "restricted_gateway_identity",
            "observed gateway class is unknown",
        ),
    }
}

fn no_silent_frontier_fallback_check(bundle: &CampaignBundle) -> CampaignCheck {
    if bundle.requested.class != GatewayClass::RestrictedCompany {
        return CampaignCheck::fail(
            "no_silent_frontier_fallback",
            "enterprise campaigns must request a restricted company gateway",
        );
    }
    if is_frontier(&bundle.observed) {
        return CampaignCheck::fail(
            "no_silent_frontier_fallback",
            format!(
                "restricted route `{}` silently served frontier identity `{}` ({})",
                bundle.requested.profile_id, bundle.observed.profile_id, bundle.observed.base_url
            ),
        );
    }
    if bundle.requested.profile_id != bundle.observed.profile_id
        || bundle.requested.model_id != bundle.observed.model_id
    {
        return CampaignCheck::fail(
            "no_silent_frontier_fallback",
            "observed profile or model drifted from the recorded restricted route",
        );
    }
    CampaignCheck::pass(
        "no_silent_frontier_fallback",
        "observed identity stayed on the recorded restricted company route",
    )
}

fn quota_receipt_check(bundle: &CampaignBundle) -> CampaignCheck {
    if bundle.quota.request_id != bundle.requested_request_id() {
        return CampaignCheck::fail(
            "quota_provider_receipt",
            "quota receipt request_id does not match the campaign request",
        );
    }
    match &bundle.quota.truth {
        QuotaTruth::Unknown => CampaignCheck::fail(
            "quota_provider_receipt",
            "quota truth is unknown; fail closed without a provider receipt",
        ),
        QuotaTruth::Contradictory { detail } => CampaignCheck::fail(
            "quota_provider_receipt",
            format!("quota receipts contradict: {detail}"),
        ),
        QuotaTruth::ProviderReceipt {
            used,
            remaining,
            limit,
            source,
            evidence_kind,
            ..
        } => {
            if source != "provider" {
                return CampaignCheck::fail(
                    "quota_provider_receipt",
                    format!("quota source `{source}` is not an explicit provider receipt"),
                );
            }
            if *evidence_kind == EvidenceKind::LiveCampaign
                && !url_can_be_live(&bundle.observed.base_url)
            {
                return CampaignCheck::fail(
                    "quota_provider_receipt",
                    "loopback or fixture URLs cannot carry live quota evidence",
                );
            }
            if let (Some(remaining), Some(limit)) = (*remaining, *limit) {
                if remaining > limit || used.saturating_add(remaining) != limit {
                    return CampaignCheck::fail(
                        "quota_provider_receipt",
                        format!(
                            "quota used={used} remaining={remaining} limit={limit} is contradictory"
                        ),
                    );
                }
            }
            CampaignCheck::pass(
                "quota_provider_receipt",
                format!("explicit provider receipt ({evidence_kind:?})"),
            )
        }
    }
}

fn retry_idempotency_check(bundle: &CampaignBundle) -> CampaignCheck {
    if bundle.attempts.is_empty() {
        return CampaignCheck::fail(
            "idempotent_retry_receipts",
            "no request/attempt/outcome receipts were recorded",
        );
    }
    let mut seen_attempts = HashMap::<u32, &AttemptReceipt>::new();
    let expected_id = bundle.requested_request_id();
    let mut first_hash: Option<&str> = None;
    let mut first_terminal: Option<AttemptOutcome> = None;
    for (index, receipt) in bundle.attempts.iter().enumerate() {
        let expected_attempt = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if receipt.attempt != expected_attempt {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                format!(
                    "attempt receipts must be contiguous starting at 1; expected {expected_attempt} got {}",
                    receipt.attempt
                ),
            );
        }
        if receipt.request_id != expected_id {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "attempt receipt request_id drifted from the campaign request",
            );
        }
        if receipt.payload_hash.len() != 64 {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "payload hash must be a 64-character SHA-256 hex digest",
            );
        }
        if seen_attempts.insert(receipt.attempt, receipt).is_some() {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                format!("duplicate attempt {}", receipt.attempt),
            );
        }
        match first_hash {
            None => first_hash = Some(receipt.payload_hash.as_str()),
            Some(hash) if hash != receipt.payload_hash => {
                return CampaignCheck::fail(
                    "idempotent_retry_receipts",
                    "request_id reused with a different payload hash",
                );
            }
            Some(_) => {}
        }
        if receipt.attempt == 1 {
            if matches!(receipt.outcome, AttemptOutcome::Replayed) {
                return CampaignCheck::fail(
                    "idempotent_retry_receipts",
                    "first attempt cannot be a replay",
                );
            }
            first_terminal = Some(receipt.outcome);
        } else if receipt.outcome != AttemptOutcome::Replayed {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "identical retries must replay the original outcome",
            );
        }
        if receipt.outcome == AttemptOutcome::Failed && receipt.error.is_none() {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "failed attempts must carry a bounded error envelope",
            );
        }
        if matches!(
            first_terminal,
            Some(AttemptOutcome::Failed) if receipt.error.is_none()
        ) {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "replay of a failure must retain the bounded error envelope",
            );
        }
    }
    CampaignCheck::pass(
        "idempotent_retry_receipts",
        format!(
            "{} auditable attempt receipts with stable payload hash",
            bundle.attempts.len()
        ),
    )
}

fn redaction_least_privilege_check(bundle: &CampaignBundle) -> CampaignCheck {
    let serialized = match serde_json::to_string(bundle) {
        Ok(value) => value,
        Err(_) => {
            return CampaignCheck::fail(
                "bounded_errors_and_redaction",
                "campaign bundle could not be serialized for redaction review",
            );
        }
    };
    let lowered = serialized.to_ascii_lowercase();
    for needle in [
        "api_key",
        "authorization",
        "bearer ",
        "credential_ref",
        "keychain:",
        "sk-",
        "cursor_api",
    ] {
        if lowered.contains(needle) {
            return CampaignCheck::fail(
                "bounded_errors_and_redaction",
                format!("campaign evidence leaked privileged token `{needle}`"),
            );
        }
    }
    for receipt in &bundle.attempts {
        if let Some(error) = &receipt.error {
            if error.message.contains("://") || error.message.contains("api_key") {
                return CampaignCheck::fail(
                    "bounded_errors_and_redaction",
                    "bounded errors must not include provider URLs or credentials",
                );
            }
            if error.event_range.is_some() {
                return CampaignCheck::fail(
                    "bounded_errors_and_redaction",
                    "provider errors must not carry privileged event ranges",
                );
            }
        }
    }
    CampaignCheck::pass(
        "bounded_errors_and_redaction",
        "errors stay inside ErrorEnvelope and omit credentials",
    )
}

fn promotion_refuses_absent_live_check(bundle: &CampaignBundle) -> CampaignCheck {
    let gates = remaining_live_gates(bundle);
    let claimed_live = bundle.promotion.live_gateway == EvidenceKind::LiveCampaign
        || bundle.promotion.live_quota == EvidenceKind::LiveCampaign
        || bundle.promotion.live_cursor_account == EvidenceKind::LiveCampaign;
    if claimed_live && !gates.is_empty() {
        return CampaignCheck::fail(
            "release_promotion_gate",
            format!(
                "promotion claimed live evidence while gates remain: {}",
                gates.join(", ")
            ),
        );
    }
    if gates.is_empty() {
        CampaignCheck::pass(
            "release_promotion_gate",
            "live gateway, quota, and Cursor-account evidence are all present",
        )
    } else {
        CampaignCheck::pass(
            "release_promotion_gate",
            format!("refusing release qualification until: {}", gates.join(", ")),
        )
    }
}

fn remaining_live_gates(bundle: &CampaignBundle) -> Vec<String> {
    let mut gates = Vec::new();
    if bundle.promotion.live_gateway != EvidenceKind::LiveCampaign
        || !url_can_be_live(&bundle.observed.base_url)
        || is_frontier(&bundle.observed)
    {
        gates.push("live restricted-company gateway campaign".into());
    }
    match &bundle.quota.truth {
        QuotaTruth::ProviderReceipt {
            source,
            evidence_kind: EvidenceKind::LiveCampaign,
            ..
        } if source == "provider"
            && bundle.promotion.live_quota == EvidenceKind::LiveCampaign
            && url_can_be_live(&bundle.observed.base_url) => {}
        _ => gates.push("live provider quota receipt".into()),
    }
    if bundle.cursor_account.provider != ExternalWorkerProvider::CursorCloud
        || bundle.cursor_account.kind != EvidenceKind::LiveCampaign
        || bundle.promotion.live_cursor_account != EvidenceKind::LiveCampaign
        || !url_can_be_live(&bundle.observed.base_url)
    {
        gates.push("live Cursor-account campaign".into());
    }
    gates
}

fn validate_identity_record(record: &GatewayIdentityRecord) -> Result<(), String> {
    if record.class == GatewayClass::Frontier {
        if record.profile_id != XAI_PROVIDER_ID {
            return Err("frontier identity must use the reserved xai profile".into());
        }
    } else {
        normalized_profile_id(&record.profile_id)?;
    }
    if record.model_id.trim().is_empty() || record.model_id.len() > 256 {
        return Err("model id must be non-empty and bounded".into());
    }
    validate_base_url(&record.base_url)?;
    if record
        .tenant
        .as_ref()
        .is_some_and(|tenant| tenant.trim().is_empty() || tenant.len() > 256)
    {
        return Err("tenant label must be non-empty and bounded".into());
    }
    Ok(())
}

fn is_frontier(record: &GatewayIdentityRecord) -> bool {
    record.class == GatewayClass::Frontier
        || record.provider_kind == ProviderKind::Xai
        || record.profile_id == XAI_PROVIDER_ID
        || frontier_host(&record.base_url)
}

fn frontier_host(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    url.host_str().is_some_and(|host| {
        let host = host.to_ascii_lowercase();
        host == "api.x.ai" || host.ends_with(".x.ai")
    })
}

fn url_can_be_live(base_url: &str) -> bool {
    if validate_base_url(base_url).is_err() {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    !loopback && !frontier_host(base_url)
}

fn validate_request_id(request_id: &str) -> Result<(), &'static str> {
    if request_id.trim().is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
        return Err("request_id must be non-empty and bounded");
    }
    Ok(())
}

impl CampaignBundle {
    fn requested_request_id(&self) -> &str {
        self.quota.request_id.as_str()
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn redact_public_text(text: &str) -> String {
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static URLS: OnceLock<Regex> = OnceLock::new();
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r#"(?i)\bbearer\s+[^\s"',;]+"#).expect("valid bearer redaction")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(r#"(?i)(api[_-]?key|token|secret|authorization)\s*[:=]\s*\S+"#)
            .expect("valid assignment redaction")
    });
    let urls = URLS.get_or_init(|| Regex::new(r#"https?://\S+"#).expect("valid url redaction"));
    let mut out = bearer.replace_all(text, "Bearer [redacted]").into_owned();
    out = assignment.replace_all(&out, "$1=[redacted]").into_owned();
    urls.replace_all(&out, "provider endpoint").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restricted_https() -> GatewayIdentityRecord {
        GatewayIdentityRecord {
            profile_id: "corp-restricted".into(),
            base_url: "https://gw.example.internal/v1".into(),
            model_id: "company-code-small".into(),
            tenant: Some("acme-tenant".into()),
            class: GatewayClass::RestrictedCompany,
            provider_kind: ProviderKind::OpenAiCompatible,
        }
    }

    fn live_shaped_bundle() -> CampaignBundle {
        let identity = restricted_https();
        CampaignBundle {
            schema: ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA.into(),
            requested: identity.clone(),
            observed: identity,
            quota: QuotaReceipt {
                request_id: "req-live-shape".into(),
                truth: QuotaTruth::ProviderReceipt {
                    used: 3,
                    remaining: Some(7),
                    limit: Some(10),
                    unit: "requests".into(),
                    source: "provider".into(),
                    evidence_kind: EvidenceKind::LiveCampaign,
                },
            },
            attempts: vec![
                AttemptReceipt {
                    request_id: "req-live-shape".into(),
                    attempt: 1,
                    payload_hash: campaign_payload_hash("req-live-shape", "review"),
                    outcome: AttemptOutcome::Succeeded,
                    error: None,
                },
                AttemptReceipt {
                    request_id: "req-live-shape".into(),
                    attempt: 2,
                    payload_hash: campaign_payload_hash("req-live-shape", "review"),
                    outcome: AttemptOutcome::Replayed,
                    error: None,
                },
            ],
            cursor_account: CursorAccountEvidence {
                provider: ExternalWorkerProvider::CursorCloud,
                kind: EvidenceKind::LiveCampaign,
            },
            promotion: ReleasePromotionEvidence {
                live_gateway: EvidenceKind::LiveCampaign,
                live_quota: EvidenceKind::LiveCampaign,
                live_cursor_account: EvidenceKind::LiveCampaign,
            },
        }
    }

    #[test]
    fn offline_restricted_fixture_passes_contract_and_refuses_release() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1");
        let bundle = gateway
            .collect_offline_campaign("req-offline", "restricted review")
            .unwrap();
        let verdict = verify_campaign(&bundle);
        assert!(verdict.contract_passed, "{verdict:#?}");
        assert!(!verdict.qualified_for_release);
        assert!(verdict
            .remaining_live_gates
            .iter()
            .any(|gate| gate.contains("gateway")));
        assert!(verdict
            .remaining_live_gates
            .iter()
            .any(|gate| gate.contains("quota")));
        assert!(verdict
            .remaining_live_gates
            .iter()
            .any(|gate| gate.contains("Cursor-account")));
        assert_eq!(bundle.attempts[1].outcome, AttemptOutcome::Replayed);
        assert_eq!(bundle.observed.class, GatewayClass::RestrictedCompany);
    }

    #[test]
    fn silent_frontier_fallback_fails_closed() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_silent_frontier_fallback();
        let bundle = gateway
            .collect_offline_campaign("req-fallback", "restricted review")
            .unwrap();
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        assert!(!verdict.qualified_for_release);
        let fallback = verdict
            .checks
            .iter()
            .find(|check| check.name == "no_silent_frontier_fallback")
            .unwrap();
        assert!(!fallback.passed);
        assert!(fallback.detail.contains("frontier"));
    }

    #[test]
    fn unknown_quota_fails_closed() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_quota_mode(FakeQuotaMode::Unknown);
        let bundle = gateway
            .collect_offline_campaign("req-quota-unknown", "review")
            .unwrap();
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(!quota.passed);
        assert!(quota.detail.contains("unknown"));
    }

    #[test]
    fn contradictory_quota_fails_closed() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_quota_mode(FakeQuotaMode::Contradictory);
        let bundle = gateway
            .collect_offline_campaign("req-quota-contradict", "review")
            .unwrap();
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(!quota.passed);
        assert!(quota.detail.contains("contradictory"));
    }

    #[test]
    fn local_inferred_usage_is_not_a_provider_receipt() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_quota_mode(FakeQuotaMode::LocalInference);
        let bundle = gateway
            .collect_offline_campaign("req-quota-local", "review")
            .unwrap();
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(quota.detail.contains("local_session"));
    }

    #[test]
    fn payload_drift_is_not_a_silent_new_request() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1");
        gateway.probe("req-drift", "first").unwrap();
        let error = gateway.probe("req-drift", "second").unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(error.message.contains("different payload"));
        assert!(!error.message.contains("secret"));
    }

    #[test]
    fn unavailable_provider_returns_redacted_bounded_error() {
        let secret = "sk-live-secret-value";
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_unavailable()
            .with_leaked_secret(secret);
        let error = gateway.probe("req-down", "review").unwrap_err();
        assert_eq!(error.code, ErrorCode::AuthorityUnavailable);
        assert_eq!(error.reason_code.as_deref(), Some("provider_unavailable"));
        assert!(!error.message.contains(secret));
        assert!(!error.message.contains("127.0.0.1"));
        let retry = gateway.probe("req-down", "review").unwrap_err();
        assert_eq!(retry.code, ErrorCode::AuthorityUnavailable);
        assert_eq!(retry.request_id.as_deref(), Some("req-down"));
    }

    #[test]
    fn claiming_live_on_loopback_fixture_refuses_qualification() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1");
        let mut bundle = gateway
            .collect_offline_campaign("req-false-live", "review")
            .unwrap();
        bundle.promotion.live_gateway = EvidenceKind::LiveCampaign;
        bundle.promotion.live_quota = EvidenceKind::LiveCampaign;
        bundle.promotion.live_cursor_account = EvidenceKind::LiveCampaign;
        bundle.cursor_account.kind = EvidenceKind::LiveCampaign;
        if let QuotaTruth::ProviderReceipt { evidence_kind, .. } = &mut bundle.quota.truth {
            *evidence_kind = EvidenceKind::LiveCampaign;
        }
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.qualified_for_release);
        assert!(verdict
            .remaining_live_gates
            .iter()
            .any(|gate| gate.contains("gateway")));
        let promotion = verdict
            .checks
            .iter()
            .find(|check| check.name == "release_promotion_gate")
            .unwrap();
        assert!(
            !promotion.passed || !verdict.contract_passed,
            "loopback cannot be advertised as live: {verdict:#?}"
        );
    }

    #[test]
    fn schema_fixture_with_complete_live_fields_would_qualify() {
        // Schema-only positive path. This is not a live company-gateway campaign.
        let verdict = verify_campaign(&live_shaped_bundle());
        assert!(verdict.contract_passed, "{verdict:#?}");
        assert!(verdict.qualified_for_release);
        assert!(verdict.remaining_live_gates.is_empty());
    }

    #[test]
    fn bounded_error_redacts_bearer_and_assignment_secrets() {
        let error = bounded_provider_error(
            ErrorCode::Internal,
            "Authorization: Bearer abc.def and api_key=super-secret",
            "req-redact",
            Some("super-secret"),
        );
        assert!(!error.message.contains("abc.def"));
        assert!(!error.message.contains("super-secret"));
        assert!(error.message.contains("[redacted]"));
    }
}
