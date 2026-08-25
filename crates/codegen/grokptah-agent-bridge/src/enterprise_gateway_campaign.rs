//! Deterministic enterprise-gateway campaign evidence.
//!
//! This module records restricted-company gateway identity, quota receipts,
//! retry attempts, Cursor-account presence, and release/promotion gates. It
//! never contacts a live company gateway, invents credentials, or treats an
//! offline fixture or hand-labeled `LiveCampaign` as live proof. A passing
//! fixture makes the live-evidence requirements explicit; it does not qualify
//! a release.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use grokptah_agent_sdk::run::MAX_REQUEST_ID_BYTES;
use grokptah_agent_sdk::{ErrorCode, ErrorEnvelope, ExternalWorkerProvider};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::external_worker::CURSOR_CLOUD_API_BASE;
use crate::gateway_config::{
    normalized_profile_id, validate_base_url, ProviderKind, XAI_PROVIDER_ID,
};

/// Contract identifier for enterprise-gateway campaign bundles.
pub const ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA: &str = "grokptah.enterprise-gateway-campaign.v1";

const PUBLIC_ERROR_NEEDLES: &[&str] = &[
    "api_key",
    "authorization",
    "bearer",
    "[redacted]",
    "credential",
    "sk-",
    "keychain:",
    "cursor_api",
];

/// How an evidence field was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// No receipt exists. Release and quota gates fail closed.
    #[default]
    Absent,
    /// Loopback or scripted provider. Never live proof.
    OfflineFixture,
    /// Named live campaign against a non-loopback company/Cursor route.
    /// Hand-labeling this kind is not verifier evidence.
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

/// One provider quota receipt bound to a request and route identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaReceipt {
    pub request_id: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub provider_kind: ProviderKind,
    pub profile_id: String,
    pub model_id: String,
    pub truth: QuotaTruth,
}

/// Outcome of one auditable attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    Failed,
    Replayed,
    Pending,
    Uncertain,
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
    /// Cursor Cloud API base. Live receipts must equal [`CURSOR_CLOUD_API_BASE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Stable Cursor run or campaign identifier. Never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
}

/// Release/promotion evidence kinds. Live fields must not be inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleasePromotionEvidence {
    pub live_gateway: EvidenceKind,
    pub live_quota: EvidenceKind,
    pub live_cursor_account: EvidenceKind,
    #[serde(default)]
    pub live_https_retry: EvidenceKind,
    #[serde(default)]
    pub live_release_artifact: EvidenceKind,
}

/// One campaign bundle evaluated by [`verify_campaign`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignBundle {
    pub schema: String,
    pub request_id: String,
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
    /// True only when this verifier independently evidences live gateway,
    /// quota, Cursor-account, HTTPS retry/idempotency, and release-artifact
    /// gates. Offline fixtures and hand-labeled `LiveCampaign` never set this.
    pub qualified_for_release: bool,
    pub checks: Vec<CampaignCheck>,
    pub remaining_live_gates: Vec<String>,
}

/// In-process fake restricted gateway used by offline tests.
#[derive(Debug)]
pub struct FakeRestrictedGateway {
    requested: GatewayIdentityRecord,
    identity: GatewayIdentityRecord,
    quota_mode: FakeQuotaMode,
    ledger_mode: FakeLedgerMode,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeLedgerMode {
    Ready,
    Unavailable,
    Pending,
    Uncertain,
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
        let identity = GatewayIdentityRecord {
            profile_id: "corp-restricted".into(),
            base_url: base_url.into(),
            model_id: "company-code-small".into(),
            tenant: Some("acme-tenant".into()),
            class: GatewayClass::RestrictedCompany,
            provider_kind: ProviderKind::OpenAiCompatible,
        };
        Self {
            requested: identity.clone(),
            identity,
            quota_mode: FakeQuotaMode::ProviderReceipt,
            ledger_mode: FakeLedgerMode::Ready,
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
        self.ledger_mode = FakeLedgerMode::Unavailable;
        self
    }

    #[must_use]
    pub fn with_pending(mut self) -> Self {
        self.ledger_mode = FakeLedgerMode::Pending;
        self
    }

    #[must_use]
    pub fn with_uncertain(mut self) -> Self {
        self.ledger_mode = FakeLedgerMode::Uncertain;
        self
    }

    #[must_use]
    pub fn with_leaked_secret(mut self, secret: impl Into<String>) -> Self {
        self.leak_secret = Some(secret.into());
        self
    }

    pub fn requested_identity(&self) -> GatewayIdentityRecord {
        self.requested.clone()
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

        match self.ledger_mode {
            FakeLedgerMode::Unavailable => {
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
                let quota = self.route_bound_quota(request_id, QuotaTruth::Unknown);
                attempts.insert(
                    request_id.into(),
                    StoredAttempt {
                        payload_hash: payload_hash.clone(),
                        quota,
                        error: Some(error.clone()),
                        count: 1,
                    },
                );
                return Err(error);
            }
            FakeLedgerMode::Pending | FakeLedgerMode::Uncertain => {
                let outcome = if self.ledger_mode == FakeLedgerMode::Pending {
                    AttemptOutcome::Pending
                } else {
                    AttemptOutcome::Uncertain
                };
                let error = bounded_ledger_status_error(outcome, request_id);
                let quota = self.route_bound_quota(request_id, QuotaTruth::Unknown);
                let attempt = AttemptReceipt {
                    request_id: request_id.into(),
                    attempt: 1,
                    payload_hash: payload_hash.clone(),
                    outcome,
                    error: Some(error.clone()),
                };
                attempts.insert(
                    request_id.into(),
                    StoredAttempt {
                        payload_hash,
                        quota: quota.clone(),
                        error: Some(error),
                        count: 1,
                    },
                );
                return Ok((self.identity.clone(), quota, attempt));
            }
            FakeLedgerMode::Ready => {}
        }

        let quota = self.route_bound_quota(
            request_id,
            match self.quota_mode {
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
        );
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
    ///
    /// Unavailable, pending, and uncertain probes are retained as attempt
    /// receipts so [`verify_campaign`] can prove fail-closed behavior. Only
    /// invalid request identity fails this collector.
    pub fn collect_offline_campaign(
        &self,
        request_id: &str,
        payload: &str,
    ) -> Result<CampaignBundle, ErrorEnvelope> {
        validate_request_id(request_id).map_err(|message| {
            bounded_provider_error(ErrorCode::InvalidRequest, message, request_id, None)
        })?;
        let requested = self.requested_identity();
        let payload_hash = campaign_payload_hash(request_id, payload);
        let first_probe = self.probe(request_id, payload);
        let retry_probe = self.probe(request_id, payload);
        let (observed, quota, first) = match first_probe {
            Ok(result) => result,
            Err(error) => {
                let first = AttemptReceipt {
                    request_id: request_id.into(),
                    attempt: 1,
                    payload_hash: payload_hash.clone(),
                    outcome: AttemptOutcome::Failed,
                    error: Some(error),
                };
                (
                    self.identity.clone(),
                    self.route_bound_quota(request_id, QuotaTruth::Unknown),
                    first,
                )
            }
        };
        let retry = match retry_probe {
            Ok((_, _, retry)) => retry,
            Err(error) => AttemptReceipt {
                request_id: request_id.into(),
                attempt: 2,
                payload_hash,
                outcome: AttemptOutcome::Replayed,
                error: Some(error),
            },
        };
        Ok(CampaignBundle {
            schema: ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA.into(),
            request_id: request_id.into(),
            requested,
            observed,
            quota,
            attempts: vec![first, retry],
            cursor_account: CursorAccountEvidence {
                provider: ExternalWorkerProvider::CursorCloud,
                kind: EvidenceKind::Absent,
                api_base: None,
                campaign_id: None,
            },
            promotion: ReleasePromotionEvidence {
                live_gateway: EvidenceKind::Absent,
                live_quota: EvidenceKind::Absent,
                live_cursor_account: EvidenceKind::Absent,
                live_https_retry: EvidenceKind::Absent,
                live_release_artifact: EvidenceKind::Absent,
            },
        })
    }

    fn route_bound_quota(&self, request_id: &str, truth: QuotaTruth) -> QuotaReceipt {
        QuotaReceipt {
            request_id: request_id.into(),
            base_url: self.identity.base_url.clone(),
            tenant: self.identity.tenant.clone(),
            provider_kind: self.identity.provider_kind,
            profile_id: self.identity.profile_id.clone(),
            model_id: self.identity.model_id.clone(),
            truth,
        }
    }
}

/// SHA-256 identity for one campaign payload. JSON is canonicalized first.
pub fn campaign_payload_hash(request_id: &str, payload: &str) -> String {
    let canonical = canonicalize_campaign_payload(payload);
    let mut hasher = Sha256::new();
    hasher.update(request_id.as_bytes());
    hasher.update([0xff]);
    hasher.update(canonical.as_bytes());
    hex_sha256(&hasher.finalize())
}

/// Share-safe provider error. Raw bodies and secrets never cross this boundary.
pub fn bounded_provider_error(
    code: ErrorCode,
    message: &str,
    request_id: &str,
    leaked_secret: Option<&str>,
) -> ErrorEnvelope {
    let _ = redact_internal_diagnostics(message, leaked_secret);
    ErrorEnvelope {
        code,
        message: public_provider_message(code, None).into(),
        request_id: Some(request_id.into()),
        reason_code: match code {
            ErrorCode::AuthorityUnavailable => Some("provider_unavailable".into()),
            ErrorCode::Capacity => Some("rate_limited".into()),
            ErrorCode::InvalidRequest => Some("invalid_request".into()),
            ErrorCode::ForbiddenScope => Some("forbidden_scope".into()),
            ErrorCode::StaleOrRecovery => Some("uncertain".into()),
            _ => Some("provider_error".into()),
        },
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
        cursor_account_receipt_check(bundle),
        retry_idempotency_check(bundle),
        redaction_least_privilege_check(bundle),
        promotion_refuses_absent_live_check(bundle),
    ];

    let contract_passed = checks.iter().all(|check| check.passed);
    let remaining_live_gates = remaining_live_gates(bundle);
    let qualified_for_release = contract_passed && remaining_live_gates.is_empty();

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
    if bundle.request_id.trim().is_empty() || bundle.request_id.len() > MAX_REQUEST_ID_BYTES {
        return CampaignCheck::fail(
            "restricted_gateway_identity",
            "bundle request_id must be non-empty and bounded",
        );
    }
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
                "restricted route `{}` silently served frontier identity `{}`",
                bundle.requested.profile_id, bundle.observed.profile_id
            ),
        );
    }
    if !same_route(&bundle.requested, &bundle.observed) {
        return CampaignCheck::fail(
            "no_silent_frontier_fallback",
            "observed base URL, tenant, provider kind, profile, or model drifted from the recorded restricted route",
        );
    }
    CampaignCheck::pass(
        "no_silent_frontier_fallback",
        "observed identity stayed on the recorded restricted company route",
    )
}

fn quota_receipt_check(bundle: &CampaignBundle) -> CampaignCheck {
    if bundle.quota.request_id != bundle.request_id {
        return CampaignCheck::fail(
            "quota_provider_receipt",
            "quota receipt request_id does not match the campaign request",
        );
    }
    if !quota_matches_identity(&bundle.quota, &bundle.requested)
        || !quota_matches_identity(&bundle.quota, &bundle.observed)
    {
        return CampaignCheck::fail(
            "quota_provider_receipt",
            "quota receipt is not bound to the bundle route identity",
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
            if *evidence_kind == EvidenceKind::LiveCampaign {
                return CampaignCheck::fail(
                    "quota_provider_receipt",
                    "hand-labeled LiveCampaign quota is not a live provider receipt",
                );
            }
            if *evidence_kind != EvidenceKind::OfflineFixture {
                return CampaignCheck::fail(
                    "quota_provider_receipt",
                    "quota receipts in this verifier must be labeled offline_fixture",
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

fn cursor_account_receipt_check(bundle: &CampaignBundle) -> CampaignCheck {
    let cursor = &bundle.cursor_account;
    if cursor.provider != ExternalWorkerProvider::CursorCloud {
        return CampaignCheck::fail(
            "cursor_account_receipt",
            "Cursor-account evidence must use the Cursor Cloud provider family",
        );
    }
    match cursor.kind {
        EvidenceKind::Absent => {
            if cursor.api_base.is_some() || cursor.campaign_id.is_some() {
                return CampaignCheck::fail(
                    "cursor_account_receipt",
                    "absent Cursor evidence must not carry an API base or campaign id",
                );
            }
            CampaignCheck::pass(
                "cursor_account_receipt",
                "Cursor-account evidence is honestly absent",
            )
        }
        EvidenceKind::OfflineFixture => {
            if cursor_uses_company_gateway(cursor, bundle) {
                return CampaignCheck::fail(
                    "cursor_account_receipt",
                    "Cursor-account receipts must not use the company gateway URL",
                );
            }
            if cursor.api_base.as_deref().is_some_and(|base| {
                normalize_api_base(base) == normalize_api_base(CURSOR_CLOUD_API_BASE)
            }) {
                return CampaignCheck::fail(
                    "cursor_account_receipt",
                    "offline Cursor fixtures cannot use the live Cursor API base",
                );
            }
            if cursor
                .api_base
                .as_deref()
                .is_some_and(|base| url_can_be_live(base))
            {
                return CampaignCheck::fail(
                    "cursor_account_receipt",
                    "offline Cursor fixtures cannot carry a non-loopback API base",
                );
            }
            CampaignCheck::pass(
                "cursor_account_receipt",
                "offline Cursor fixture is secret-free and not a live receipt",
            )
        }
        EvidenceKind::LiveCampaign => {
            if let Err(detail) = validate_live_cursor_receipt(cursor, bundle) {
                return CampaignCheck::fail("cursor_account_receipt", detail);
            }
            CampaignCheck::fail(
                "cursor_account_receipt",
                "hand-labeled LiveCampaign is not a live Cursor-account receipt",
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
    let expected_id = bundle.request_id.as_str();
    let mut first_hash: Option<&str> = None;
    let mut first_terminal: Option<AttemptOutcome> = None;
    let mut first_error: Option<ErrorEnvelope> = None;
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
            if receipt.outcome == AttemptOutcome::Succeeded && receipt.error.is_some() {
                return CampaignCheck::fail(
                    "idempotent_retry_receipts",
                    "succeeded attempts must not carry an error envelope",
                );
            }
            first_terminal = Some(receipt.outcome);
            first_error = receipt.error.clone();
        } else if receipt.outcome != AttemptOutcome::Replayed {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "identical retries must replay the original outcome",
            );
        } else if receipt.error != first_error {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "replayed success/error envelope drifted from the original attempt",
            );
        }
        if matches!(
            receipt.outcome,
            AttemptOutcome::Failed | AttemptOutcome::Pending | AttemptOutcome::Uncertain
        ) && receipt.error.is_none()
        {
            return CampaignCheck::fail(
                "idempotent_retry_receipts",
                "failed, pending, and uncertain attempts must carry a bounded error envelope",
            );
        }
        if let Some(error) = &receipt.error {
            if let Err(detail) = public_error_is_needle_free(error) {
                return CampaignCheck::fail("idempotent_retry_receipts", detail);
            }
        }
    }
    if matches!(
        first_terminal,
        Some(AttemptOutcome::Pending | AttemptOutcome::Uncertain)
    ) {
        return CampaignCheck::fail(
            "idempotent_retry_receipts",
            "pending/uncertain outcomes fail closed until reconciled",
        );
    }
    CampaignCheck::pass(
        "idempotent_retry_receipts",
        format!(
            "{} auditable attempt receipts with canonical payload hash and matching replay envelopes",
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
                "campaign evidence leaked a privileged token",
            );
        }
    }
    for receipt in &bundle.attempts {
        if let Some(error) = &receipt.error {
            if let Err(detail) = public_error_is_needle_free(error) {
                return CampaignCheck::fail("bounded_errors_and_redaction", detail);
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
        "public errors stay inside ErrorEnvelope and are needle-free",
    )
}

fn promotion_refuses_absent_live_check(bundle: &CampaignBundle) -> CampaignCheck {
    let gates = remaining_live_gates(bundle);
    let claimed_live = bundle.promotion.live_gateway == EvidenceKind::LiveCampaign
        || bundle.promotion.live_quota == EvidenceKind::LiveCampaign
        || bundle.promotion.live_cursor_account == EvidenceKind::LiveCampaign
        || bundle.promotion.live_https_retry == EvidenceKind::LiveCampaign
        || bundle.promotion.live_release_artifact == EvidenceKind::LiveCampaign;
    if claimed_live {
        return CampaignCheck::fail(
            "release_promotion_gate",
            format!(
                "hand-labeled LiveCampaign fields are not verifier evidence; remaining live gates: {}",
                gates.join(", ")
            ),
        );
    }
    CampaignCheck::pass(
        "release_promotion_gate",
        format!("refusing release qualification until: {}", gates.join(", ")),
    )
}

fn remaining_live_gates(_bundle: &CampaignBundle) -> Vec<String> {
    // This verifier never contacts a live gateway, Cursor account, or release
    // artifact store. Schema labels therefore cannot clear these gates.
    vec![
        "live restricted-company gateway campaign".into(),
        "live provider quota receipt".into(),
        "live Cursor-account campaign".into(),
        "live HTTPS retry/idempotency".into(),
        "release artifact from the reviewed head".into(),
    ]
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

fn same_route(left: &GatewayIdentityRecord, right: &GatewayIdentityRecord) -> bool {
    left.profile_id == right.profile_id
        && left.base_url == right.base_url
        && left.model_id == right.model_id
        && left.tenant == right.tenant
        && left.class == right.class
        && left.provider_kind == right.provider_kind
}

fn quota_matches_identity(quota: &QuotaReceipt, identity: &GatewayIdentityRecord) -> bool {
    quota.base_url == identity.base_url
        && quota.tenant == identity.tenant
        && quota.provider_kind == identity.provider_kind
        && quota.profile_id == identity.profile_id
        && quota.model_id == identity.model_id
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

fn normalize_api_base(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn cursor_uses_company_gateway(cursor: &CursorAccountEvidence, bundle: &CampaignBundle) -> bool {
    let Some(api_base) = cursor.api_base.as_deref() else {
        return false;
    };
    let normalized = normalize_api_base(api_base);
    normalized == normalize_api_base(&bundle.requested.base_url)
        || normalized == normalize_api_base(&bundle.observed.base_url)
}

fn validate_live_cursor_receipt(
    cursor: &CursorAccountEvidence,
    bundle: &CampaignBundle,
) -> Result<(), String> {
    let api_base = cursor
        .api_base
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("live Cursor receipts require CURSOR_CLOUD_API_BASE")?;
    if normalize_api_base(api_base) != normalize_api_base(CURSOR_CLOUD_API_BASE) {
        return Err("live Cursor receipts must use CURSOR_CLOUD_API_BASE".into());
    }
    if cursor_uses_company_gateway(cursor, bundle) {
        return Err("Cursor-account receipts must not use the company gateway URL".into());
    }
    let campaign_id = cursor
        .campaign_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("live Cursor receipts require a stable run/campaign identifier")?;
    validate_stable_campaign_id(campaign_id)?;
    if public_text_contains_needle(campaign_id) {
        return Err("Cursor campaign id must be secret-free".into());
    }
    Ok(())
}

fn validate_stable_campaign_id(campaign_id: &str) -> Result<(), String> {
    if campaign_id.len() > MAX_REQUEST_ID_BYTES {
        return Err("Cursor campaign id must be bounded".into());
    }
    if !campaign_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("Cursor campaign id must be a stable identifier".into());
    }
    Ok(())
}

fn validate_request_id(request_id: &str) -> Result<(), &'static str> {
    if request_id.trim().is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
        return Err("request_id must be non-empty and bounded");
    }
    Ok(())
}

fn public_provider_message(code: ErrorCode, reason: Option<&str>) -> &'static str {
    match reason {
        Some("pending") => "The provider request is pending.",
        Some("uncertain") => "The provider request outcome is uncertain.",
        _ => match code {
            ErrorCode::AuthorityUnavailable => "The requested provider is unavailable.",
            ErrorCode::Capacity => "The provider is at capacity.",
            ErrorCode::InvalidRequest => "The request is invalid.",
            ErrorCode::ForbiddenScope => "The requested scope is not allowed.",
            ErrorCode::Unauthenticated => "The request is not authenticated.",
            ErrorCode::NotFound => "The requested resource was not found.",
            ErrorCode::StaleOrRecovery => "The provider request outcome is uncertain.",
            ErrorCode::Internal => "The provider request failed.",
        },
    }
}

fn bounded_ledger_status_error(outcome: AttemptOutcome, request_id: &str) -> ErrorEnvelope {
    let reason = match outcome {
        AttemptOutcome::Pending => "pending",
        AttemptOutcome::Uncertain => "uncertain",
        _ => "provider_error",
    };
    ErrorEnvelope {
        code: ErrorCode::StaleOrRecovery,
        message: public_provider_message(ErrorCode::StaleOrRecovery, Some(reason)).into(),
        request_id: Some(request_id.into()),
        reason_code: Some(reason.into()),
        event_range: None,
    }
}

fn canonicalize_campaign_payload(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|_| payload.to_string()),
        Err(_) => payload.to_string(),
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

fn redact_internal_diagnostics(text: &str, leaked_secret: Option<&str>) -> String {
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
    out = urls.replace_all(&out, "provider endpoint").into_owned();
    if let Some(secret) = leaked_secret.filter(|value| !value.is_empty()) {
        out = out.replace(secret, "[redacted]");
    }
    out
}

fn public_text_contains_needle(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    PUBLIC_ERROR_NEEDLES
        .iter()
        .any(|needle| lowered.contains(needle))
        || text.contains("://")
}

fn public_error_is_needle_free(error: &ErrorEnvelope) -> Result<(), String> {
    if public_text_contains_needle(&error.message) {
        return Err("public ErrorEnvelope message must be needle-free".into());
    }
    if error
        .reason_code
        .as_deref()
        .is_some_and(public_text_contains_needle)
    {
        return Err("public ErrorEnvelope reason must be needle-free".into());
    }
    Ok(())
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

    fn route_bound_quota(
        request_id: &str,
        identity: &GatewayIdentityRecord,
        truth: QuotaTruth,
    ) -> QuotaReceipt {
        QuotaReceipt {
            request_id: request_id.into(),
            base_url: identity.base_url.clone(),
            tenant: identity.tenant.clone(),
            provider_kind: identity.provider_kind,
            profile_id: identity.profile_id.clone(),
            model_id: identity.model_id.clone(),
            truth,
        }
    }

    fn live_shaped_bundle() -> CampaignBundle {
        let identity = restricted_https();
        CampaignBundle {
            schema: ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA.into(),
            request_id: "req-live-shape".into(),
            requested: identity.clone(),
            observed: identity.clone(),
            quota: route_bound_quota(
                "req-live-shape",
                &identity,
                QuotaTruth::ProviderReceipt {
                    used: 3,
                    remaining: Some(7),
                    limit: Some(10),
                    unit: "requests".into(),
                    source: "provider".into(),
                    evidence_kind: EvidenceKind::LiveCampaign,
                },
            ),
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
                api_base: Some(CURSOR_CLOUD_API_BASE.into()),
                campaign_id: Some("bc-live-campaign-1".into()),
            },
            promotion: ReleasePromotionEvidence {
                live_gateway: EvidenceKind::LiveCampaign,
                live_quota: EvidenceKind::LiveCampaign,
                live_cursor_account: EvidenceKind::LiveCampaign,
                live_https_retry: EvidenceKind::LiveCampaign,
                live_release_artifact: EvidenceKind::LiveCampaign,
            },
        }
    }

    fn remaining_gate_names(verdict: &CampaignVerdict) -> Vec<&str> {
        verdict
            .remaining_live_gates
            .iter()
            .map(String::as_str)
            .collect()
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
        assert_eq!(
            remaining_gate_names(&verdict),
            [
                "live restricted-company gateway campaign",
                "live provider quota receipt",
                "live Cursor-account campaign",
                "live HTTPS retry/idempotency",
                "release artifact from the reviewed head",
            ]
        );
        assert_eq!(bundle.attempts[1].outcome, AttemptOutcome::Replayed);
        assert_eq!(bundle.observed.class, GatewayClass::RestrictedCompany);
        assert_eq!(bundle.quota.request_id, bundle.request_id);
        assert_eq!(bundle.quota.base_url, bundle.observed.base_url);
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
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(!quota.passed);
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
        assert_eq!(error.reason_code.as_deref(), Some("invalid_request"));
        assert_eq!(error.message, "The request is invalid.");
        assert!(public_error_is_needle_free(&error).is_ok());
    }

    #[test]
    fn unavailable_provider_returns_needle_free_public_error() {
        let secret = "sk-live-secret-value";
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_unavailable()
            .with_leaked_secret(secret);
        let error = gateway.probe("req-down", "review").unwrap_err();
        assert_eq!(error.code, ErrorCode::AuthorityUnavailable);
        assert_eq!(error.reason_code.as_deref(), Some("provider_unavailable"));
        assert_eq!(error.message, "The requested provider is unavailable.");
        assert!(public_error_is_needle_free(&error).is_ok());
        assert!(!error.message.contains(secret));
        assert!(!error.message.contains("[redacted]"));
        let retry = gateway.probe("req-down", "review").unwrap_err();
        assert_eq!(retry, error);
    }

    #[test]
    fn collect_unavailable_campaign_is_verified_fail_closed() {
        let secret = "sk-live-secret-value";
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_unavailable()
            .with_leaked_secret(secret);
        let bundle = gateway
            .collect_offline_campaign("req-down-collect", "review")
            .unwrap();
        assert_eq!(bundle.attempts[0].outcome, AttemptOutcome::Failed);
        assert_eq!(bundle.attempts[1].outcome, AttemptOutcome::Replayed);
        assert_eq!(
            bundle.attempts[0].error.as_ref(),
            bundle.attempts[1].error.as_ref()
        );
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        assert!(!verdict.qualified_for_release);
        let redaction = verdict
            .checks
            .iter()
            .find(|check| check.name == "bounded_errors_and_redaction")
            .unwrap();
        assert!(redaction.passed, "{verdict:#?}");
        let retry = verdict
            .checks
            .iter()
            .find(|check| check.name == "idempotent_retry_receipts")
            .unwrap();
        assert!(retry.passed, "{verdict:#?}");
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(!quota.passed);
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(!serialized.contains(secret));
        assert!(!serialized.contains("[redacted]"));
        assert!(!serialized.to_ascii_lowercase().contains("api_key"));
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
        bundle.cursor_account.api_base = Some(bundle.observed.base_url.clone());
        bundle.cursor_account.campaign_id = Some("loopback-claim".into());
        if let QuotaTruth::ProviderReceipt { evidence_kind, .. } = &mut bundle.quota.truth {
            *evidence_kind = EvidenceKind::LiveCampaign;
        }
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.qualified_for_release);
        assert_eq!(verdict.remaining_live_gates.len(), 5);
        let promotion = verdict
            .checks
            .iter()
            .find(|check| check.name == "release_promotion_gate")
            .unwrap();
        assert!(
            !promotion.passed,
            "loopback cannot be advertised as live: {verdict:#?}"
        );
        let cursor = verdict
            .checks
            .iter()
            .find(|check| check.name == "cursor_account_receipt")
            .unwrap();
        assert!(!cursor.passed);
        assert!(
            cursor.detail.contains("CURSOR_CLOUD_API_BASE") || cursor.detail.contains("company")
        );
    }

    #[test]
    fn schema_fixture_with_complete_live_fields_does_not_qualify() {
        let verdict = verify_campaign(&live_shaped_bundle());
        assert!(!verdict.qualified_for_release);
        assert!(!verdict.contract_passed, "{verdict:#?}");
        assert_eq!(verdict.remaining_live_gates.len(), 5);
        let promotion = verdict
            .checks
            .iter()
            .find(|check| check.name == "release_promotion_gate")
            .unwrap();
        assert!(!promotion.passed);
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(!quota.passed);
        let cursor = verdict
            .checks
            .iter()
            .find(|check| check.name == "cursor_account_receipt")
            .unwrap();
        assert!(!cursor.passed);
        assert!(cursor.detail.contains("hand-labeled"));
    }

    #[test]
    fn public_error_is_needle_free_while_internal_diagnostics_stay_redacted() {
        let error = bounded_provider_error(
            ErrorCode::Internal,
            "Authorization: Bearer abc.def and api_key=super-secret https://evil.example/v1",
            "req-redact",
            Some("super-secret"),
        );
        assert_eq!(error.message, "The provider request failed.");
        assert!(public_error_is_needle_free(&error).is_ok());
        let internal = redact_internal_diagnostics(
            "Authorization: Bearer abc.def and api_key=super-secret https://evil.example/v1",
            Some("super-secret"),
        );
        assert!(internal.contains("[redacted]"));
        assert!(!internal.contains("abc.def"));
        assert!(!internal.contains("super-secret"));
        assert!(!internal.contains("https://"));
        assert_ne!(error.message, internal);
    }

    #[test]
    fn quota_route_drift_fails_closed() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1");
        let mut bundle = gateway
            .collect_offline_campaign("req-quota-drift", "review")
            .unwrap();
        bundle.quota.base_url = "https://gw.example.internal/v1".into();
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        let quota = verdict
            .checks
            .iter()
            .find(|check| check.name == "quota_provider_receipt")
            .unwrap();
        assert!(!quota.passed);
        assert!(quota.detail.contains("route identity"));
    }

    #[test]
    fn tenant_and_provider_kind_drift_fails_closed() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1");
        let mut bundle = gateway
            .collect_offline_campaign("req-tenant-drift", "review")
            .unwrap();
        bundle.observed.tenant = Some("other-tenant".into());
        let verdict = verify_campaign(&bundle);
        assert!(!verdict.contract_passed);
        let fallback = verdict
            .checks
            .iter()
            .find(|check| check.name == "no_silent_frontier_fallback")
            .unwrap();
        assert!(!fallback.passed);
    }

    #[test]
    fn cursor_live_claim_requires_cursor_api_base_and_campaign_id() {
        let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1");
        let mut bundle = gateway
            .collect_offline_campaign("req-cursor-shape", "review")
            .unwrap();
        bundle.cursor_account.kind = EvidenceKind::LiveCampaign;
        let missing = verify_campaign(&bundle);
        let cursor = missing
            .checks
            .iter()
            .find(|check| check.name == "cursor_account_receipt")
            .unwrap();
        assert!(!cursor.passed);
        assert!(cursor.detail.contains("CURSOR_CLOUD_API_BASE"));

        bundle.cursor_account.api_base = Some(CURSOR_CLOUD_API_BASE.into());
        let missing_id = verify_campaign(&bundle);
        let cursor = missing_id
            .checks
            .iter()
            .find(|check| check.name == "cursor_account_receipt")
            .unwrap();
        assert!(!cursor.passed);
        assert!(cursor.detail.contains("campaign"));
    }

    #[test]
    fn cursor_receipt_cannot_use_company_gateway_url() {
        let identity = restricted_https();
        let mut bundle = live_shaped_bundle();
        bundle.quota.truth = QuotaTruth::ProviderReceipt {
            used: 3,
            remaining: Some(7),
            limit: Some(10),
            unit: "requests".into(),
            source: "provider".into(),
            evidence_kind: EvidenceKind::OfflineFixture,
        };
        bundle.promotion = ReleasePromotionEvidence {
            live_gateway: EvidenceKind::Absent,
            live_quota: EvidenceKind::Absent,
            live_cursor_account: EvidenceKind::Absent,
            live_https_retry: EvidenceKind::Absent,
            live_release_artifact: EvidenceKind::Absent,
        };
        bundle.cursor_account.api_base = Some(identity.base_url.clone());
        let verdict = verify_campaign(&bundle);
        let cursor = verdict
            .checks
            .iter()
            .find(|check| check.name == "cursor_account_receipt")
            .unwrap();
        assert!(!cursor.passed);
        assert!(
            cursor.detail.contains("CURSOR_CLOUD_API_BASE")
                || cursor.detail.contains("company gateway")
        );
        assert!(!verdict.qualified_for_release);
    }

    #[test]
    fn json_payload_hash_is_canonical() {
        let left = campaign_payload_hash("req-canon", r#"{"b":2,"a":1}"#);
        let right = campaign_payload_hash("req-canon", r#"{"a":1,"b":2}"#);
        let spaced = campaign_payload_hash("req-canon", r#"{ "a": 1, "b": 2 }"#);
        assert_eq!(left, right);
        assert_eq!(left, spaced);
        assert_ne!(
            campaign_payload_hash("req-canon", r#"{"a":1}"#),
            campaign_payload_hash("req-canon", r#"{"a":2}"#)
        );
    }

    #[test]
    fn replayed_error_envelopes_must_match() {
        let gateway =
            FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1").with_unavailable();
        let mut bundle = gateway
            .collect_offline_campaign("req-replay-drift", "review")
            .unwrap();
        let drifted = bounded_provider_error(
            ErrorCode::Capacity,
            "different public failure",
            "req-replay-drift",
            None,
        );
        bundle.attempts[1].error = Some(drifted);
        let verdict = verify_campaign(&bundle);
        let retry = verdict
            .checks
            .iter()
            .find(|check| check.name == "idempotent_retry_receipts")
            .unwrap();
        assert!(!retry.passed);
        assert!(retry.detail.contains("envelope"));
    }

    #[test]
    fn pending_and_uncertain_fail_closed() {
        let pending = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_pending()
            .collect_offline_campaign("req-pending", "review")
            .unwrap();
        assert_eq!(pending.attempts[0].outcome, AttemptOutcome::Pending);
        assert_eq!(pending.attempts[1].outcome, AttemptOutcome::Replayed);
        assert_eq!(
            pending.attempts[0]
                .error
                .as_ref()
                .unwrap()
                .reason_code
                .as_deref(),
            Some("pending")
        );
        let pending_verdict = verify_campaign(&pending);
        assert!(!pending_verdict.contract_passed);
        assert!(!pending_verdict.qualified_for_release);
        let retry = pending_verdict
            .checks
            .iter()
            .find(|check| check.name == "idempotent_retry_receipts")
            .unwrap();
        assert!(!retry.passed);
        assert!(retry.detail.contains("pending"));

        let uncertain = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:9/v1")
            .with_uncertain()
            .collect_offline_campaign("req-uncertain", "review")
            .unwrap();
        assert_eq!(uncertain.attempts[0].outcome, AttemptOutcome::Uncertain);
        let uncertain_verdict = verify_campaign(&uncertain);
        assert!(!uncertain_verdict.contract_passed);
        let retry = uncertain_verdict
            .checks
            .iter()
            .find(|check| check.name == "idempotent_retry_receipts")
            .unwrap();
        assert!(retry.detail.contains("uncertain"));
        assert!(public_error_is_needle_free(uncertain.attempts[0].error.as_ref().unwrap()).is_ok());
    }
}
