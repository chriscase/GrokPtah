//! Provider execution authority and durable continuation.
//!
//! Every physical provider request an Agent makes is a privileged action: it
//! spends the account's money, speaks with the account's credential, and can
//! mutate the repository the Agent is bound to. This module makes that
//! authority explicit and durable.
//!
//! Three invariants hold here and are enforced fail-closed:
//!
//! 1. **Bound authority.** An attempt may only be admitted against an
//!    authority-owned scope (account, tenant/installation, agent, run, lane,
//!    frozen specification revision, provider route, credential class, model,
//!    and repository/ref/policy). A binding that is missing a field,
//!    disagrees with the authority-owned scope, has expired, or replays a
//!    request fingerprint is denied.
//! 2. **Single-use confirmation.** Transport is gated on a confirmation grant
//!    that is audience-checked against the exact binding digest, nonce
//!    checked in constant time, expiry checked, and durably consumed *before*
//!    the transition it authorizes. A restart cannot resurrect a spent grant.
//! 3. **Honest delivery state.** Intent and request identity are persisted
//!    before transport, so the durable record can always distinguish
//!    `known_not_sent`, `sending`, `uncertain`, and `settled`. An `uncertain`
//!    attempt is never automatically retried; only an explicit reconciliation
//!    may move it to `settled`.
//!
//! The ledger deliberately stores identifiers, classes, and digests. It never
//! stores credentials, endpoint URLs, prompts, or provider prose, so its
//! receipt projection is safe to hand to an operator or a coordinator.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::certification::{CredentialMethodClass, ProviderRouteClass};

use super::authz::constant_time_eq;
use super::store::{atomic_write_json, write_json_exclusive};
use super::types::{
    hash_payload, safe_id_filename, AgentAuthorityPolicy, AgentModelSpec, AgentRecord, AgentSpec,
    OrchError, OrchErrorCode, RunRecord, RunStopCause,
};

pub const PROVIDER_AUTHORITY_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_ATTEMPT_RECEIPT_SCHEMA: &str = "grokptah.provider_attempt_receipt.v1";

/// Bounded string ceiling for every caller-supplied binding field.
pub const MAX_PROVIDER_BINDING_FIELD_BYTES: usize = 512;
/// Ledger ceiling for the bounded per-attempt transition history.
pub const MAX_PROVIDER_ATTEMPT_TRANSITIONS: usize = 32;
/// Ledger ceiling for attempts retained per run receipt projection.
pub const MAX_PROVIDER_RECEIPTS_PER_RUN: usize = 512;
/// Default lifetime of an authority binding before it is considered stale.
pub const DEFAULT_BINDING_TTL_MS: i64 = 5 * 60 * 1000;
/// Default lifetime of a single-use confirmation grant.
pub const DEFAULT_GRANT_TTL_MS: i64 = 2 * 60 * 1000;
/// Minimum entropy accepted for a confirmation nonce.
pub const MIN_CONFIRMATION_NONCE_BYTES: usize = 32;

// ---------------------------------------------------------------------------
// Denials
// ---------------------------------------------------------------------------

/// Typed fail-closed denial reason for a provider authority decision.
///
/// These map onto the existing [`OrchErrorCode`] wire contract rather than
/// widening it, and travel in `error.data.denial` so a coordinator can branch
/// on the exact boundary that refused the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthorityDenial {
    /// A required binding field was absent or empty.
    BindingMissing,
    /// The claimed binding disagrees with the authority-owned scope.
    BindingMismatch,
    /// The binding is expired, or froze a superseded specification revision.
    BindingStale,
    /// The request fingerprint was already claimed by another attempt.
    BindingReplayed,
    /// The binding names a provider route the authority does not own.
    RouteNotAuthorized,
    /// The binding crosses an account, tenant, or installation boundary.
    TenantMismatch,
    /// The binding crosses a repository, ref, or policy boundary.
    RepositoryMismatch,
    /// The binding names a model outside the frozen Agent route.
    ModelMismatch,
    /// Transport was attempted with no confirmation grant.
    GrantMissing,
    /// The grant was minted for a different binding digest.
    GrantAudienceMismatch,
    /// The grant was minted for a different attempt.
    GrantSubjectMismatch,
    /// The grant is past its expiry.
    GrantExpired,
    /// The presented nonce does not match the grant.
    GrantNonceMismatch,
    /// The grant was already spent; grants are single-use across restart.
    GrantAlreadyConsumed,
    /// The requested send-state transition is not part of the lattice.
    SendStateTransitionInvalid,
    /// An `uncertain` attempt may never be automatically retried.
    UncertainAttemptNotRetryable,
    /// A prior attempt still holds this continuation key.
    ContinuationKeyBusy,
    /// The attempt id is unknown to the ledger.
    AttemptUnknown,
    /// The attempt id is already present in the ledger.
    AttemptAlreadyExists,
    /// A structural ledger bound was exceeded.
    LedgerBoundExceeded,
}

impl ProviderAuthorityDenial {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BindingMissing => "binding_missing",
            Self::BindingMismatch => "binding_mismatch",
            Self::BindingStale => "binding_stale",
            Self::BindingReplayed => "binding_replayed",
            Self::RouteNotAuthorized => "route_not_authorized",
            Self::TenantMismatch => "tenant_mismatch",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::ModelMismatch => "model_mismatch",
            Self::GrantMissing => "grant_missing",
            Self::GrantAudienceMismatch => "grant_audience_mismatch",
            Self::GrantSubjectMismatch => "grant_subject_mismatch",
            Self::GrantExpired => "grant_expired",
            Self::GrantNonceMismatch => "grant_nonce_mismatch",
            Self::GrantAlreadyConsumed => "grant_already_consumed",
            Self::SendStateTransitionInvalid => "send_state_transition_invalid",
            Self::UncertainAttemptNotRetryable => "uncertain_attempt_not_retryable",
            Self::ContinuationKeyBusy => "continuation_key_busy",
            Self::AttemptUnknown => "attempt_unknown",
            Self::AttemptAlreadyExists => "attempt_already_exists",
            Self::LedgerBoundExceeded => "ledger_bound_exceeded",
        }
    }

    /// Existing control-plane error code this denial reports as. New denial
    /// reasons never widen [`OrchErrorCode`]; they refine it through `data`.
    pub const fn error_code(self) -> OrchErrorCode {
        match self {
            Self::BindingMissing | Self::SendStateTransitionInvalid => {
                OrchErrorCode::InvalidRequest
            }
            Self::BindingStale => OrchErrorCode::StaleVersion,
            Self::BindingReplayed
            | Self::GrantAlreadyConsumed
            | Self::ContinuationKeyBusy
            | Self::UncertainAttemptNotRetryable
            | Self::AttemptAlreadyExists => OrchErrorCode::Conflict,
            Self::AttemptUnknown => OrchErrorCode::InvalidRequest,
            Self::LedgerBoundExceeded => OrchErrorCode::CapacityExhausted,
            Self::RepositoryMismatch => OrchErrorCode::WorkspaceMismatch,
            _ => OrchErrorCode::ForbiddenScope,
        }
    }

    pub fn into_error(self, detail: impl Into<String>) -> OrchError {
        OrchError::with_data(
            self.error_code(),
            detail.into(),
            serde_json::json!({ "denial": self.as_str() }),
        )
    }
}

fn deny<T>(denial: ProviderAuthorityDenial, detail: impl Into<String>) -> Result<T, OrchError> {
    Err(denial.into_error(detail))
}

// ---------------------------------------------------------------------------
// Send-state lattice
// ---------------------------------------------------------------------------

/// Whether a provider request physically reached the provider.
///
/// This is deliberately distinct from the run/attempt *outcome*. A settled
/// attempt may have failed; an uncertain attempt may in fact have succeeded.
/// Only `known_not_sent` supports an automatic retry, because only there does
/// the host actually know the provider never saw the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSendState {
    /// Durably admitted, intent recorded, transport not started.
    KnownNotSent,
    /// Transport started; delivery is in flight.
    Sending,
    /// Transport started and the outcome cannot be established.
    Uncertain,
    /// Delivery and outcome are both established.
    Settled,
}

impl ProviderSendState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotSent => "known_not_sent",
            Self::Sending => "sending",
            Self::Uncertain => "uncertain",
            Self::Settled => "settled",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Settled)
    }

    /// Whether the host may transparently re-issue this logical request.
    ///
    /// `uncertain` is never auto-retryable: re-sending could double-charge the
    /// account or duplicate a side effect the provider already applied.
    pub const fn auto_retry_allowed(self) -> bool {
        matches!(self, Self::KnownNotSent)
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::KnownNotSent, Self::Sending)
                | (Self::KnownNotSent, Self::Settled)
                | (Self::Sending, Self::Settled)
                | (Self::Sending, Self::Uncertain)
                | (Self::Uncertain, Self::Settled)
        )
    }
}

/// Why an attempt's delivery or outcome could not be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUncertaintyReason {
    /// The process died between starting transport and observing a response.
    RestartDuringTransport,
    /// The connection dropped after the request bytes were committed.
    TransportInterrupted,
    /// The wall-clock deadline elapsed with no usable response.
    DeadlineElapsed,
    /// The response arrived but could not be reconciled with the request.
    ResponseUnreconcilable,
}

impl ProviderUncertaintyReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RestartDuringTransport => "restart_during_transport",
            Self::TransportInterrupted => "transport_interrupted",
            Self::DeadlineElapsed => "deadline_elapsed",
            Self::ResponseUnreconcilable => "response_unreconcilable",
        }
    }
}

/// Established terminal disposition of an attempt whose delivery is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSettledOutcome {
    /// The provider returned a usable response.
    Delivered,
    /// The provider rejected the request before doing model work.
    Rejected,
    /// The host abandoned the attempt before any transport occurred.
    AbandonedBeforeSend,
    /// The operator or host cancelled the attempt.
    Cancelled,
    /// An earlier `uncertain` attempt was reconciled against provider records.
    ReconciledDelivered,
    /// An earlier `uncertain` attempt was reconciled as never delivered.
    ReconciledNotDelivered,
}

impl ProviderSettledOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Rejected => "rejected",
            Self::AbandonedBeforeSend => "abandoned_before_send",
            Self::Cancelled => "cancelled",
            Self::ReconciledDelivered => "reconciled_delivered",
            Self::ReconciledNotDelivered => "reconciled_not_delivered",
        }
    }

    /// Outcomes that may only be reached by explicit reconciliation of an
    /// `uncertain` attempt.
    pub const fn is_reconciliation(self) -> bool {
        matches!(
            self,
            Self::ReconciledDelivered | Self::ReconciledNotDelivered
        )
    }

    /// Whether the request is known to have reached the provider.
    pub const fn delivered(self) -> bool {
        matches!(
            self,
            Self::Delivered | Self::Rejected | Self::ReconciledDelivered
        )
    }
}

/// A fact this receipt explicitly does not know.
///
/// Unknowns are enumerated rather than defaulted so a reader never mistakes a
/// missing field for a negative answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUnknown {
    /// The provider never revealed its own request identifier.
    ProviderRequestId,
    /// Whether the request reached the provider is not established.
    Delivery,
    /// The provider's outcome for this request is not established.
    ProviderOutcome,
    /// Token accounting for this attempt is unavailable.
    Usage,
}

impl ProviderUnknown {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderRequestId => "provider_request_id",
            Self::Delivery => "delivery",
            Self::ProviderOutcome => "provider_outcome",
            Self::Usage => "usage",
        }
    }
}

// ---------------------------------------------------------------------------
// Authority scope and binding
// ---------------------------------------------------------------------------

/// Repository/ref/policy half of a provider authority scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRepositoryBinding {
    /// Canonical source workspace owned by the Agent specification.
    pub workspace: String,
    /// Repository revision or ref frozen for this attempt.
    pub repository_ref: String,
    /// Digest over the effective authority policy in force for this attempt.
    pub policy_digest: String,
}

impl ProviderRepositoryBinding {
    fn validate(&self) -> Result<(), OrchError> {
        bounded_field(&self.workspace, "repository.workspace")?;
        bounded_field(&self.repository_ref, "repository.repositoryRef")?;
        bounded_field(&self.policy_digest, "repository.policyDigest")?;
        Ok(())
    }

    fn digest(&self) -> String {
        hash_payload(&serde_json::json!({
            "workspace": self.workspace,
            "repositoryRef": self.repository_ref,
            "policyDigest": self.policy_digest,
        }))
    }
}

/// The authority-owned truth a provider attempt must agree with.
///
/// This is derived by the host from durable records (the authenticated
/// principal, the Agent record and its frozen specification revision, the run,
/// and the qualified provider route). It is never taken from a caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthorityScope {
    /// Account identity that owns the Agent across device credentials.
    pub owner_principal_id: String,
    /// Tenant the account acts within.
    pub tenant_id: String,
    /// Installation of that tenant this host process serves.
    pub installation_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub lane_id: Uuid,
    /// Frozen Agent specification revision authorizing this attempt.
    pub agent_spec_revision: u64,
    pub model: AgentModelSpec,
    pub route_class: ProviderRouteClass,
    /// Digest of the normalized endpoint. The endpoint is never stored.
    pub endpoint_fingerprint: String,
    pub credential_method: CredentialMethodClass,
    /// Digest of the credential identity. The credential is never stored.
    pub credential_binding_digest: String,
    pub repository: ProviderRepositoryBinding,
}

impl ProviderAuthorityScope {
    pub fn validate(&self) -> Result<(), OrchError> {
        bounded_field(&self.owner_principal_id, "authority.ownerPrincipalId")?;
        bounded_field(&self.tenant_id, "authority.tenantId")?;
        bounded_field(&self.installation_id, "authority.installationId")?;
        bounded_field(&self.agent_id, "authority.agentId")?;
        bounded_field(&self.run_id, "authority.runId")?;
        bounded_field(&self.endpoint_fingerprint, "authority.endpointFingerprint")?;
        bounded_field(
            &self.credential_binding_digest,
            "authority.credentialBindingDigest",
        )?;
        if self.agent_spec_revision == 0 {
            return deny(
                ProviderAuthorityDenial::BindingMissing,
                "authority.agentSpecRevision must be greater than zero",
            );
        }
        bounded_field(&self.model.selection_key, "authority.model.selectionKey")?;
        bounded_field(&self.model.provider_id, "authority.model.providerId")?;
        bounded_field(&self.model.model_id, "authority.model.modelId")?;
        self.repository.validate()?;
        Ok(())
    }

    /// Stable digest over the whole authority scope, used as the confirmation
    /// grant audience so a grant can never be replayed across tenants,
    /// repositories, routes, models, or specification revisions.
    pub fn digest(&self) -> String {
        hash_payload(&serde_json::json!({
            "schemaVersion": PROVIDER_AUTHORITY_SCHEMA_VERSION,
            "ownerPrincipalId": self.owner_principal_id,
            "tenantId": self.tenant_id,
            "installationId": self.installation_id,
            "agentId": self.agent_id,
            "runId": self.run_id,
            "laneId": self.lane_id,
            "agentSpecRevision": self.agent_spec_revision,
            "modelSelectionKey": self.model.selection_key,
            "modelProviderId": self.model.provider_id,
            "modelId": self.model.model_id,
            "routeClass": self.route_class,
            "endpointFingerprint": self.endpoint_fingerprint,
            "credentialMethod": self.credential_method,
            "credentialBindingDigest": self.credential_binding_digest,
            "repositoryDigest": self.repository.digest(),
        }))
    }

    /// Ownership half of the scope. Control operations on an existing attempt
    /// re-check this so a later specification revision cannot strand a record,
    /// while a different account, tenant, installation, agent, run, or
    /// repository can still never reach it.
    fn ownership_digest(&self) -> String {
        hash_payload(&serde_json::json!({
            "schemaVersion": PROVIDER_AUTHORITY_SCHEMA_VERSION,
            "ownerPrincipalId": self.owner_principal_id,
            "tenantId": self.tenant_id,
            "installationId": self.installation_id,
            "agentId": self.agent_id,
            "runId": self.run_id,
            "workspace": self.repository.workspace,
        }))
    }

    fn first_mismatch(&self, authority: &Self) -> Option<ProviderAuthorityDenial> {
        if self.owner_principal_id != authority.owner_principal_id
            || self.tenant_id != authority.tenant_id
            || self.installation_id != authority.installation_id
        {
            return Some(ProviderAuthorityDenial::TenantMismatch);
        }
        if self.repository != authority.repository {
            return Some(ProviderAuthorityDenial::RepositoryMismatch);
        }
        if self.route_class != authority.route_class
            || self.endpoint_fingerprint != authority.endpoint_fingerprint
            || self.credential_method != authority.credential_method
            || self.credential_binding_digest != authority.credential_binding_digest
        {
            return Some(ProviderAuthorityDenial::RouteNotAuthorized);
        }
        if self.model != authority.model {
            return Some(ProviderAuthorityDenial::ModelMismatch);
        }
        if self.agent_spec_revision != authority.agent_spec_revision {
            return Some(ProviderAuthorityDenial::BindingStale);
        }
        if self.agent_id != authority.agent_id
            || self.run_id != authority.run_id
            || self.lane_id != authority.lane_id
        {
            return Some(ProviderAuthorityDenial::BindingMismatch);
        }
        None
    }
}

/// A claimed authority binding for exactly one provider attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthorityBinding {
    pub schema_version: u32,
    pub scope: ProviderAuthorityScope,
    /// Digest over the exact request this attempt will transmit. Replaying a
    /// fingerprint is a denial, so a legitimate retry must re-fingerprint.
    pub request_fingerprint: String,
    /// Stable key for the logical provider request being made. A retry of the
    /// same logical request reuses this key; the ledger refuses to open a new
    /// attempt while an `uncertain` attempt still holds it.
    pub continuation_key: String,
    pub bound_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl ProviderAuthorityBinding {
    /// Bind a provider attempt to an authority-owned scope.
    pub fn bind(
        scope: ProviderAuthorityScope,
        request_fingerprint: impl Into<String>,
        continuation_key: impl Into<String>,
        bound_at: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<Self, OrchError> {
        let binding = Self {
            schema_version: PROVIDER_AUTHORITY_SCHEMA_VERSION,
            scope,
            request_fingerprint: request_fingerprint.into(),
            continuation_key: continuation_key.into(),
            bound_at,
            expires_at: bound_at + ttl,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != PROVIDER_AUTHORITY_SCHEMA_VERSION {
            return deny(
                ProviderAuthorityDenial::BindingStale,
                "provider authority binding schema version is unsupported",
            );
        }
        self.scope.validate()?;
        bounded_field(&self.request_fingerprint, "binding.requestFingerprint")?;
        bounded_field(&self.continuation_key, "binding.continuationKey")?;
        if self.expires_at <= self.bound_at {
            return deny(
                ProviderAuthorityDenial::BindingMissing,
                "binding.expiresAt must be after binding.boundAt",
            );
        }
        Ok(())
    }

    /// Digest used as the confirmation grant audience.
    pub fn binding_digest(&self) -> String {
        hash_payload(&serde_json::json!({
            "schemaVersion": self.schema_version,
            "authorityDigest": self.scope.digest(),
            "requestFingerprint": self.request_fingerprint,
            "continuationKey": self.continuation_key,
            "boundAt": self.bound_at,
            "expiresAt": self.expires_at,
        }))
    }

    /// Full admission check: structural validity, agreement with the
    /// authority-owned scope, and freshness. Fails closed on the first
    /// boundary that refuses.
    pub fn authorize_start(
        &self,
        authority: &ProviderAuthorityScope,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        self.validate()?;
        authority.validate()?;
        if let Some(denial) = self.scope.first_mismatch(authority) {
            return deny(
                denial,
                format!(
                    "provider attempt binding is not authorized ({})",
                    denial.as_str()
                ),
            );
        }
        if now >= self.expires_at {
            return deny(
                ProviderAuthorityDenial::BindingStale,
                "provider attempt binding has expired",
            );
        }
        if now < self.bound_at {
            return deny(
                ProviderAuthorityDenial::BindingStale,
                "provider attempt binding is not yet valid",
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Durable intent and request identity
// ---------------------------------------------------------------------------

/// What the host intends to do once this attempt settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpDisposition {
    /// Nothing follows this attempt.
    #[default]
    None,
    /// The run continues with another provider round.
    ContinueRun,
    /// The run finalizes after this attempt.
    FinalizeRun,
    /// A continuation checkpoint is taken after this attempt.
    Checkpoint,
}

impl FollowUpDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ContinueRun => "continue_run",
            Self::FinalizeRun => "finalize_run",
            Self::Checkpoint => "checkpoint",
        }
    }
}

/// Durable cancellation intent for this attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelDisposition {
    #[default]
    NotRequested,
    /// Cancellation was recorded durably; transport must not start or must
    /// stop at the next safe point.
    Requested,
    /// The host observed the cancellation take effect.
    Acknowledged,
}

impl CancelDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Requested => "requested",
            Self::Acknowledged => "acknowledged",
        }
    }
}

/// Follow-up and cancel intent, persisted before transport so a crash can
/// never lose what this attempt was for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderContinuationIntent {
    #[serde(default)]
    pub follow_up: FollowUpDisposition,
    #[serde(default)]
    pub cancel: CancelDisposition,
    /// Host-authored classification for the follow-up. Never model prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at: Option<DateTime<Utc>>,
    /// Host-decided stop cause recorded alongside a cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_stop_cause: Option<RunStopCause>,
}

impl ProviderContinuationIntent {
    fn validate(&self) -> Result<(), OrchError> {
        if let Some(code) = self.follow_up_code.as_deref() {
            bounded_field(code, "intent.followUpCode")?;
        }
        if (self.cancel == CancelDisposition::NotRequested) != self.cancel_requested_at.is_none() {
            return deny(
                ProviderAuthorityDenial::BindingMissing,
                "cancellation intent and its timestamp must agree",
            );
        }
        Ok(())
    }
}

/// Request identity persisted before transport.
///
/// `client_request_id` is host-generated and therefore always known; it is the
/// idempotency key the host presents to the provider. `provider_request_id`
/// stays `None` until the provider actually reveals one, and its absence is
/// reported as an explicit unknown rather than silently omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRequestIdentity {
    pub client_request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
}

impl ProviderRequestIdentity {
    pub fn new(client_request_id: impl Into<String>) -> Self {
        Self {
            client_request_id: client_request_id.into(),
            provider_request_id: None,
        }
    }

    fn validate(&self) -> Result<(), OrchError> {
        bounded_field(&self.client_request_id, "request.clientRequestId")?;
        if let Some(id) = self.provider_request_id.as_deref() {
            bounded_field(id, "request.providerRequestId")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Confirmation grants
// ---------------------------------------------------------------------------

/// A single-use authorization to start transport for exactly one attempt.
///
/// The nonce is stored only as a digest, so the durable grant and every
/// projection of it remain secret-free. Consumption is written durably before
/// the transition it authorizes, which is what makes single use survive a
/// restart in the middle of a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmationGrant {
    pub schema_version: u32,
    pub grant_id: String,
    /// Binding digest this grant is minted for.
    pub audience: String,
    pub subject_attempt_id: String,
    /// Digest of the confirmation nonce. The nonce itself is never stored.
    pub nonce_digest: String,
    pub issued_by: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<DateTime<Utc>>,
}

impl ConfirmationGrant {
    pub fn is_consumed(&self) -> bool {
        self.consumed_at.is_some()
    }

    fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != PROVIDER_AUTHORITY_SCHEMA_VERSION {
            return deny(
                ProviderAuthorityDenial::GrantMissing,
                "confirmation grant schema version is unsupported",
            );
        }
        bounded_field(&self.grant_id, "grant.grantId")?;
        bounded_field(&self.audience, "grant.audience")?;
        bounded_field(&self.subject_attempt_id, "grant.subjectAttemptId")?;
        bounded_field(&self.nonce_digest, "grant.nonceDigest")?;
        bounded_field(&self.issued_by, "grant.issuedBy")?;
        if self.expires_at <= self.issued_at {
            return deny(
                ProviderAuthorityDenial::GrantExpired,
                "grant.expiresAt must be after grant.issuedAt",
            );
        }
        Ok(())
    }
}

/// Digest of a confirmation nonce. Keeping this in one place ensures the
/// issuing and presenting sides agree byte for byte.
pub fn confirmation_nonce_digest(nonce: &str) -> String {
    hash_payload(&serde_json::json!({ "confirmationNonce": nonce }))
}

/// Mint a fresh confirmation nonce with sufficient entropy.
pub fn new_confirmation_nonce() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

// ---------------------------------------------------------------------------
// Durable attempt record
// ---------------------------------------------------------------------------

/// One durable send-state transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSendTransition {
    pub from: ProviderSendState,
    pub to: ProviderSendState,
    pub at: DateTime<Utc>,
    /// Bounded host-authored classification. Never model prose.
    pub cause: String,
}

/// The durable provider attempt record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptRecord {
    pub schema_version: u32,
    pub attempt_id: String,
    pub binding: ProviderAuthorityBinding,
    pub request: ProviderRequestIdentity,
    pub intent: ProviderContinuationIntent,
    pub send_state: ProviderSendState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty_reason: Option<ProviderUncertaintyReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_outcome: Option<ProviderSettledOutcome>,
    /// Grant that authorized transport, once one has been consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorizing_grant_id: Option<String>,
    #[serde(default)]
    pub transitions: Vec<ProviderSendTransition>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProviderAttemptRecord {
    /// Facts this record explicitly does not know, derived from the lattice
    /// rather than stored, so they can never drift from the send state.
    pub fn unknowns(&self) -> Vec<ProviderUnknown> {
        let mut unknowns = Vec::new();
        if self.request.provider_request_id.is_none() {
            unknowns.push(ProviderUnknown::ProviderRequestId);
        }
        match self.send_state {
            ProviderSendState::KnownNotSent => {
                unknowns.push(ProviderUnknown::ProviderOutcome);
                unknowns.push(ProviderUnknown::Usage);
            }
            ProviderSendState::Sending | ProviderSendState::Uncertain => {
                unknowns.push(ProviderUnknown::Delivery);
                unknowns.push(ProviderUnknown::ProviderOutcome);
                unknowns.push(ProviderUnknown::Usage);
            }
            ProviderSendState::Settled => {
                if !self
                    .settled_outcome
                    .is_some_and(ProviderSettledOutcome::delivered)
                {
                    unknowns.push(ProviderUnknown::Usage);
                }
            }
        }
        unknowns.sort_unstable();
        unknowns.dedup();
        unknowns
    }

    /// Whether the host may transparently re-issue this logical request.
    pub fn auto_retry_allowed(&self) -> bool {
        self.send_state.auto_retry_allowed()
    }

    /// Bounded, secret-free projection for operators and coordinators.
    pub fn receipt(&self) -> ProviderAttemptReceipt {
        ProviderAttemptReceipt {
            schema: PROVIDER_ATTEMPT_RECEIPT_SCHEMA.to_string(),
            attempt_id: self.attempt_id.clone(),
            client_request_id: self.request.client_request_id.clone(),
            provider_request_id: self.request.provider_request_id.clone(),
            send_state: self.send_state,
            auto_retry_allowed: self.auto_retry_allowed(),
            authority: AuthorityBindingSummary::of(&self.binding),
            follow_up: self.intent.follow_up,
            cancel: self.intent.cancel,
            uncertainty_reason: self.uncertainty_reason,
            settled_outcome: self.settled_outcome,
            confirmed: self.authorizing_grant_id.is_some(),
            unknowns: self.unknowns(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn push_transition(&mut self, to: ProviderSendState, at: DateTime<Utc>, cause: &str) {
        if self.transitions.len() >= MAX_PROVIDER_ATTEMPT_TRANSITIONS {
            self.transitions.remove(0);
        }
        self.transitions.push(ProviderSendTransition {
            from: self.send_state,
            to,
            at,
            cause: cause.to_string(),
        });
        self.send_state = to;
        self.updated_at = at;
    }
}

/// Secret-free summary of the authority a receipt was produced under.
///
/// Raw account, tenant, installation, and workspace values are reduced to
/// digests so a receipt can be shared without leaking an identity directory or
/// a host filesystem path. Orchestration identifiers that already appear in
/// run projections are kept verbatim so the receipt is actually actionable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityBindingSummary {
    pub binding_digest: String,
    pub authority_digest: String,
    pub owner_digest: String,
    pub tenant_digest: String,
    pub installation_digest: String,
    pub agent_id: String,
    pub run_id: String,
    pub lane_id: Uuid,
    pub agent_spec_revision: u64,
    pub route_class: ProviderRouteClass,
    pub endpoint_fingerprint: String,
    pub credential_method: CredentialMethodClass,
    pub model_selection_key: String,
    pub model_provider_id: String,
    pub model_id: String,
    pub workspace_digest: String,
    pub repository_ref: String,
    pub policy_digest: String,
    pub request_fingerprint: String,
    pub continuation_key: String,
    pub bound_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl AuthorityBindingSummary {
    pub fn of(binding: &ProviderAuthorityBinding) -> Self {
        let scope = &binding.scope;
        Self {
            binding_digest: binding.binding_digest(),
            authority_digest: scope.digest(),
            owner_digest: opaque_digest("owner", &scope.owner_principal_id),
            tenant_digest: opaque_digest("tenant", &scope.tenant_id),
            installation_digest: opaque_digest("installation", &scope.installation_id),
            agent_id: scope.agent_id.clone(),
            run_id: scope.run_id.clone(),
            lane_id: scope.lane_id,
            agent_spec_revision: scope.agent_spec_revision,
            route_class: scope.route_class.clone(),
            endpoint_fingerprint: scope.endpoint_fingerprint.clone(),
            credential_method: scope.credential_method.clone(),
            model_selection_key: scope.model.selection_key.clone(),
            model_provider_id: scope.model.provider_id.clone(),
            model_id: scope.model.model_id.clone(),
            workspace_digest: opaque_digest("workspace", &scope.repository.workspace),
            repository_ref: scope.repository.repository_ref.clone(),
            policy_digest: scope.repository.policy_digest.clone(),
            request_fingerprint: binding.request_fingerprint.clone(),
            continuation_key: binding.continuation_key.clone(),
            bound_at: binding.bound_at,
            expires_at: binding.expires_at,
        }
    }
}

/// Bounded, secret-free receipt for one provider attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptReceipt {
    pub schema: String,
    pub attempt_id: String,
    pub client_request_id: String,
    /// `None` while the provider has not revealed one; the matching
    /// [`ProviderUnknown::ProviderRequestId`] entry says so explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    pub send_state: ProviderSendState,
    pub auto_retry_allowed: bool,
    pub authority: AuthorityBindingSummary,
    pub follow_up: FollowUpDisposition,
    pub cancel: CancelDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty_reason: Option<ProviderUncertaintyReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_outcome: Option<ProviderSettledOutcome>,
    /// Whether a single-use confirmation grant authorized transport.
    pub confirmed: bool,
    /// Facts this receipt explicitly does not know.
    pub unknowns: Vec<ProviderUnknown>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn opaque_digest(domain: &str, value: &str) -> String {
    hash_payload(&serde_json::json!({ "domain": domain, "value": value }))
}

fn bounded_field(value: &str, field: &str) -> Result<(), OrchError> {
    if value.trim().is_empty() {
        return deny(
            ProviderAuthorityDenial::BindingMissing,
            format!("{field} is required"),
        );
    }
    if value.len() > MAX_PROVIDER_BINDING_FIELD_BYTES || value.bytes().any(|byte| byte == 0) {
        return deny(
            ProviderAuthorityDenial::BindingMissing,
            format!("{field} exceeds its bound or contains a NUL byte"),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Durable ledger
// ---------------------------------------------------------------------------

/// Everything the ledger needs to admit one provider attempt.
#[derive(Debug, Clone)]
pub struct ProviderAttemptRequest {
    pub attempt_id: String,
    pub binding: ProviderAuthorityBinding,
    pub request: ProviderRequestIdentity,
    pub intent: ProviderContinuationIntent,
}

/// Explicit reconciliation of an `uncertain` attempt.
///
/// This exists so the only path out of `uncertain` is a deliberate decision
/// with recorded evidence, never an automatic retry.
#[derive(Debug, Clone)]
pub struct ProviderUncertaintyResolution {
    pub outcome: ProviderSettledOutcome,
    /// Provider request identity learned during reconciliation, when the
    /// provider could name one.
    pub provider_request_id: Option<String>,
    /// Bounded host-authored classification of the evidence used.
    pub evidence_code: String,
    pub resolved_by: String,
}

/// Durable, crash-safe ledger of authority-bound provider attempts.
///
/// The ledger is deliberately a separate durable domain from the run store.
/// It is opened alongside the orchestration store and performs its own restart
/// recovery, so an interrupted send can never be replayed as if it had never
/// happened.
///
/// Concurrency: the ledger lives below the orchestration store root, whose
/// exclusive advisory lock already admits one host process at a time. Within
/// that process an internal mutex serializes every read-modify-write, and
/// fingerprint claims and grant issuance additionally use exclusive
/// create-new installs so a racing writer fails closed rather than
/// overwriting.
#[derive(Clone)]
pub struct ProviderAuthorityLedger {
    inner: Arc<LedgerInner>,
}

struct LedgerInner {
    root: PathBuf,
    lock: Mutex<()>,
}

impl std::fmt::Debug for ProviderAuthorityLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAuthorityLedger")
            .field("root", &self.inner.root)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintClaim {
    run_id: String,
    attempt_id: String,
    claimed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinuationHolder {
    attempt_id: String,
    claimed_at: DateTime<Utc>,
}

impl ProviderAuthorityLedger {
    /// Open the ledger below an orchestration store root and run restart
    /// recovery. Any attempt left `sending` by a crash becomes `uncertain`.
    pub fn open(store_root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = store_root.as_ref().join("provider-authority");
        for child in ["attempts", "grants", "fingerprints", "continuation-holders"] {
            fs::create_dir_all(root.join(child))?;
        }
        let ledger = Self {
            inner: Arc::new(LedgerInner {
                root,
                lock: Mutex::new(()),
            }),
        };
        ledger.recover_after_restart()?;
        Ok(ledger)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Convert every attempt left mid-transport into `uncertain`.
    ///
    /// This is the restart half of the honest-delivery invariant: the host
    /// cannot know whether those requests reached the provider, so it must not
    /// claim they did not.
    pub fn recover_after_restart(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let now = Utc::now();
        let mut recovered = 0usize;
        let attempts_root = self.inner.root.join("attempts");
        let run_dirs = match fs::read_dir(&attempts_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        for run_dir in run_dirs {
            let run_dir = run_dir?;
            if !run_dir.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(run_dir.path())? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(mut record) = serde_json::from_str::<ProviderAttemptRecord>(&text) else {
                    continue;
                };
                if record.send_state != ProviderSendState::Sending {
                    continue;
                }
                record.uncertainty_reason = Some(ProviderUncertaintyReason::RestartDuringTransport);
                record.push_transition(
                    ProviderSendState::Uncertain,
                    now,
                    "restart_during_transport",
                );
                atomic_write_json(&path, &record)?;
                recovered += 1;
            }
        }
        Ok(recovered)
    }

    // -- paths ------------------------------------------------------------

    fn run_dir(&self, kind: &str, run_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self.inner.root.join(kind).join(safe_id_filename(run_id)?))
    }

    fn attempt_path(&self, run_id: &str, attempt_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .run_dir("attempts", run_id)?
            .join(format!("{}.json", safe_id_filename(attempt_id)?)))
    }

    fn grant_path(&self, run_id: &str, grant_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .run_dir("grants", run_id)?
            .join(format!("{}.json", safe_id_filename(grant_id)?)))
    }

    fn fingerprint_path(&self, fingerprint: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("fingerprints")
            .join(format!("{}.json", safe_id_filename(fingerprint)?)))
    }

    fn continuation_path(&self, run_id: &str, key: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .run_dir("continuation-holders", run_id)?
            .join(format!("{}.json", safe_id_filename(key)?)))
    }

    // -- admission --------------------------------------------------------

    /// Durably admit one provider attempt against an authority-owned scope.
    ///
    /// Everything a later reader needs — the binding, the follow-up and cancel
    /// intent, and the host-generated request identity — is written before the
    /// caller is allowed anywhere near transport.
    pub fn begin_attempt(
        &self,
        authority: &ProviderAuthorityScope,
        request: ProviderAttemptRequest,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        bounded_field(&request.attempt_id, "attempt.attemptId")?;
        request.binding.authorize_start(authority, now)?;
        request.request.validate()?;
        request.intent.validate()?;
        if request.intent.cancel != CancelDisposition::NotRequested {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "a cancelled attempt must not be admitted",
            );
        }
        if request.request.provider_request_id.is_some() {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "a provider request identity cannot be known before transport starts",
            );
        }

        let run_id = authority.run_id.as_str();
        let attempt_path = self.attempt_path(run_id, &request.attempt_id)?;
        let fingerprint_path = self.fingerprint_path(&request.binding.request_fingerprint)?;
        let continuation_path =
            self.continuation_path(run_id, &request.binding.continuation_key)?;

        let _guard = self.inner.lock.lock();
        if attempt_path.is_file() {
            return deny(
                ProviderAuthorityDenial::AttemptAlreadyExists,
                "provider attempt id is already present in the ledger",
            );
        }
        if self.count_attempts(run_id)? >= MAX_PROVIDER_RECEIPTS_PER_RUN {
            return deny(
                ProviderAuthorityDenial::LedgerBoundExceeded,
                "run has reached its provider attempt ledger bound",
            );
        }

        // An `uncertain` attempt keeps its continuation key. Opening a fresh
        // attempt for the same logical request would be exactly the automatic
        // retry the lattice forbids.
        if let Some(holder) = read_json::<ContinuationHolder>(&continuation_path)? {
            if let Some(previous) = self.read_attempt(run_id, &holder.attempt_id)? {
                match previous.send_state {
                    ProviderSendState::Uncertain => {
                        return deny(
                            ProviderAuthorityDenial::UncertainAttemptNotRetryable,
                            "an uncertain provider attempt holds this continuation key and \
                             must be reconciled explicitly",
                        );
                    }
                    ProviderSendState::Sending | ProviderSendState::KnownNotSent => {
                        return deny(
                            ProviderAuthorityDenial::ContinuationKeyBusy,
                            "a live provider attempt already holds this continuation key",
                        );
                    }
                    ProviderSendState::Settled => {}
                }
            }
        }

        // Replay guard. The fingerprint claim is content-addressed and
        // exclusively created, so two hosts racing the same request cannot
        // both be admitted.
        let claim = FingerprintClaim {
            run_id: run_id.to_string(),
            attempt_id: request.attempt_id.clone(),
            claimed_at: now,
        };
        match write_json_exclusive(&fingerprint_path, &claim) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return deny(
                    ProviderAuthorityDenial::BindingReplayed,
                    "provider request fingerprint was already claimed by another attempt",
                );
            }
            Err(error) => return Err(internal(error)),
        }

        let record = ProviderAttemptRecord {
            schema_version: PROVIDER_AUTHORITY_SCHEMA_VERSION,
            attempt_id: request.attempt_id,
            binding: request.binding,
            request: request.request,
            intent: request.intent,
            send_state: ProviderSendState::KnownNotSent,
            uncertainty_reason: None,
            settled_outcome: None,
            authorizing_grant_id: None,
            transitions: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        atomic_write_json(&attempt_path, &record).map_err(internal)?;
        atomic_write_json(
            &continuation_path,
            &ContinuationHolder {
                attempt_id: record.attempt_id.clone(),
                claimed_at: now,
            },
        )
        .map_err(internal)?;
        Ok(record)
    }

    // -- confirmation grants ---------------------------------------------

    /// Mint a single-use confirmation grant for exactly one admitted attempt.
    ///
    /// The caller keeps the returned nonce; the ledger keeps only its digest.
    pub fn issue_confirmation_grant(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        issued_by: &str,
        nonce: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<ConfirmationGrant, OrchError> {
        if nonce.len() < MIN_CONFIRMATION_NONCE_BYTES {
            return deny(
                ProviderAuthorityDenial::GrantNonceMismatch,
                "confirmation nonce does not carry enough entropy",
            );
        }
        let _guard = self.inner.lock.lock();
        let record = self.require_attempt(authority, attempt_id)?;
        if record.send_state != ProviderSendState::KnownNotSent {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "only an attempt that has not been sent may be confirmed",
            );
        }
        let grant = ConfirmationGrant {
            schema_version: PROVIDER_AUTHORITY_SCHEMA_VERSION,
            grant_id: Uuid::new_v4().to_string(),
            audience: record.binding.binding_digest(),
            subject_attempt_id: record.attempt_id.clone(),
            nonce_digest: confirmation_nonce_digest(nonce),
            issued_by: issued_by.to_string(),
            issued_at: now,
            expires_at: now + ttl,
            consumed_at: None,
        };
        grant.validate()?;
        let path = self.grant_path(&authority.run_id, &grant.grant_id)?;
        write_json_exclusive(&path, &grant).map_err(internal)?;
        Ok(grant)
    }

    /// Start transport for an admitted attempt, spending its confirmation.
    ///
    /// The grant is consumed durably *before* the send-state transition, so a
    /// crash between the two can only ever lose the send, never the fact that
    /// the confirmation was spent.
    pub fn begin_transport(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        grant_id: &str,
        nonce: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if record.intent.cancel != CancelDisposition::NotRequested {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "attempt has a durable cancellation intent and must not be sent",
            );
        }
        if !record
            .send_state
            .can_transition_to(ProviderSendState::Sending)
        {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                format!(
                    "cannot start transport from send state {}",
                    record.send_state.as_str()
                ),
            );
        }
        // Re-check freshness at the transport boundary: a binding that was
        // valid at admission may have gone stale while awaiting confirmation.
        record.binding.authorize_start(authority, now)?;

        let grant_path = self.grant_path(&authority.run_id, grant_id)?;
        let Some(mut grant) = read_json::<ConfirmationGrant>(&grant_path)? else {
            return deny(
                ProviderAuthorityDenial::GrantMissing,
                "confirmation grant is unknown",
            );
        };
        grant.validate()?;
        if grant.subject_attempt_id != record.attempt_id {
            return deny(
                ProviderAuthorityDenial::GrantSubjectMismatch,
                "confirmation grant was minted for a different attempt",
            );
        }
        if !constant_time_eq(
            grant.audience.as_bytes(),
            record.binding.binding_digest().as_bytes(),
        ) {
            return deny(
                ProviderAuthorityDenial::GrantAudienceMismatch,
                "confirmation grant audience does not match this binding",
            );
        }
        if grant.is_consumed() {
            return deny(
                ProviderAuthorityDenial::GrantAlreadyConsumed,
                "confirmation grant is single-use and was already spent",
            );
        }
        if now >= grant.expires_at {
            return deny(
                ProviderAuthorityDenial::GrantExpired,
                "confirmation grant has expired",
            );
        }
        if !constant_time_eq(
            confirmation_nonce_digest(nonce).as_bytes(),
            grant.nonce_digest.as_bytes(),
        ) {
            return deny(
                ProviderAuthorityDenial::GrantNonceMismatch,
                "confirmation nonce does not match the grant",
            );
        }

        grant.consumed_at = Some(now);
        atomic_write_json(&grant_path, &grant).map_err(internal)?;

        record.authorizing_grant_id = Some(grant.grant_id.clone());
        record.push_transition(ProviderSendState::Sending, now, "confirmation_consumed");
        self.write_attempt(authority, &record)?;
        Ok(record)
    }

    // -- outcome ----------------------------------------------------------

    /// Record the provider's own request identifier once it is known.
    pub fn record_provider_request_id(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        provider_request_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        bounded_field(provider_request_id, "request.providerRequestId")?;
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if record.send_state == ProviderSendState::KnownNotSent {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "a provider request identity cannot exist before transport starts",
            );
        }
        if record
            .request
            .provider_request_id
            .as_deref()
            .is_some_and(|existing| existing != provider_request_id)
        {
            return deny(
                ProviderAuthorityDenial::BindingMismatch,
                "provider request identity is already bound to a different value",
            );
        }
        record.request.provider_request_id = Some(provider_request_id.to_string());
        record.updated_at = now;
        self.write_attempt(authority, &record)?;
        Ok(record)
    }

    /// Settle an attempt whose delivery is established.
    pub fn settle_attempt(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        outcome: ProviderSettledOutcome,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        if outcome.is_reconciliation() {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "a reconciliation outcome requires explicit uncertainty resolution",
            );
        }
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if record.send_state == ProviderSendState::Uncertain {
            return deny(
                ProviderAuthorityDenial::UncertainAttemptNotRetryable,
                "an uncertain attempt may only be settled by explicit reconciliation",
            );
        }
        if !record
            .send_state
            .can_transition_to(ProviderSendState::Settled)
        {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                format!(
                    "cannot settle from send state {}",
                    record.send_state.as_str()
                ),
            );
        }
        if record.send_state == ProviderSendState::KnownNotSent
            && !matches!(
                outcome,
                ProviderSettledOutcome::AbandonedBeforeSend | ProviderSettledOutcome::Cancelled
            )
        {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "an attempt that was never sent cannot settle with a delivered outcome",
            );
        }
        if record.send_state == ProviderSendState::Sending
            && outcome == ProviderSettledOutcome::AbandonedBeforeSend
        {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "an attempt that started transport cannot settle as never sent",
            );
        }
        record.settled_outcome = Some(outcome);
        record.push_transition(ProviderSendState::Settled, now, outcome.as_str());
        self.write_attempt(authority, &record)?;
        Ok(record)
    }

    /// Mark an in-flight attempt as having an unknowable outcome.
    pub fn mark_uncertain(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        reason: ProviderUncertaintyReason,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if !record
            .send_state
            .can_transition_to(ProviderSendState::Uncertain)
        {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                format!(
                    "cannot mark send state {} as uncertain",
                    record.send_state.as_str()
                ),
            );
        }
        record.uncertainty_reason = Some(reason);
        record.push_transition(ProviderSendState::Uncertain, now, reason.as_str());
        self.write_attempt(authority, &record)?;
        Ok(record)
    }

    /// Explicitly reconcile an `uncertain` attempt against provider evidence.
    ///
    /// This is the only exit from `uncertain`, and it is never taken by the
    /// retry path.
    pub fn resolve_uncertain(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        resolution: ProviderUncertaintyResolution,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        bounded_field(&resolution.evidence_code, "resolution.evidenceCode")?;
        bounded_field(&resolution.resolved_by, "resolution.resolvedBy")?;
        if !resolution.outcome.is_reconciliation() {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "uncertainty resolution requires a reconciliation outcome",
            );
        }
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if record.send_state != ProviderSendState::Uncertain {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "only an uncertain attempt can be reconciled",
            );
        }
        if let Some(provider_request_id) = resolution.provider_request_id.as_deref() {
            bounded_field(provider_request_id, "resolution.providerRequestId")?;
            record.request.provider_request_id = Some(provider_request_id.to_string());
        }
        record.settled_outcome = Some(resolution.outcome);
        record.push_transition(ProviderSendState::Settled, now, &resolution.evidence_code);
        self.write_attempt(authority, &record)?;
        Ok(record)
    }

    /// Durably record cancellation intent for an attempt.
    ///
    /// Recording intent never sends anything and never cancels an in-flight
    /// request on its own; it makes the operator's decision survive a crash so
    /// the transport boundary can refuse to start.
    pub fn record_cancel_intent(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        stop_cause: RunStopCause,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if record.send_state.is_terminal() {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "a settled attempt cannot accept a cancellation intent",
            );
        }
        if record.intent.cancel == CancelDisposition::NotRequested {
            record.intent.cancel = CancelDisposition::Requested;
            record.intent.cancel_requested_at = Some(now);
            record.intent.cancel_stop_cause = Some(stop_cause);
            record.updated_at = now;
            self.write_attempt(authority, &record)?;
        }
        Ok(record)
    }

    /// Acknowledge that a recorded cancellation actually took effect.
    pub fn acknowledge_cancel(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut record = self.require_attempt(authority, attempt_id)?;
        if record.intent.cancel != CancelDisposition::Requested {
            return deny(
                ProviderAuthorityDenial::SendStateTransitionInvalid,
                "no cancellation intent is pending for this attempt",
            );
        }
        record.intent.cancel = CancelDisposition::Acknowledged;
        record.updated_at = now;
        self.write_attempt(authority, &record)?;
        Ok(record)
    }

    // -- reads ------------------------------------------------------------

    /// Load one attempt under the caller's authority.
    pub fn load_attempt(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        self.require_attempt(authority, attempt_id)
    }

    /// Every attempt for the caller's run, oldest first.
    pub fn list_attempts(
        &self,
        authority: &ProviderAuthorityScope,
    ) -> Result<Vec<ProviderAttemptRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        authority.validate()?;
        let dir = self.run_dir("attempts", &authority.run_id)?;
        let mut records = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(internal(error)),
        };
        let ownership = authority.ownership_digest();
        for entry in entries {
            let path = entry.map_err(internal)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(record) = read_json::<ProviderAttemptRecord>(&path)? else {
                continue;
            };
            if record.binding.scope.ownership_digest() != ownership {
                continue;
            }
            records.push(record);
        }
        records.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.attempt_id.cmp(&right.attempt_id))
        });
        Ok(records)
    }

    /// Bounded, secret-free receipts for the caller's run.
    pub fn receipts(
        &self,
        authority: &ProviderAuthorityScope,
    ) -> Result<Vec<ProviderAttemptReceipt>, OrchError> {
        Ok(self
            .list_attempts(authority)?
            .iter()
            .take(MAX_PROVIDER_RECEIPTS_PER_RUN)
            .map(ProviderAttemptRecord::receipt)
            .collect())
    }

    /// Load a confirmation grant under the caller's authority.
    pub fn load_grant(
        &self,
        authority: &ProviderAuthorityScope,
        grant_id: &str,
    ) -> Result<Option<ConfirmationGrant>, OrchError> {
        let _guard = self.inner.lock.lock();
        authority.validate()?;
        read_json(&self.grant_path(&authority.run_id, grant_id)?)
    }

    // -- internals --------------------------------------------------------

    fn count_attempts(&self, run_id: &str) -> Result<usize, OrchError> {
        let dir = self.run_dir("attempts", run_id)?;
        match fs::read_dir(&dir) {
            Ok(entries) => Ok(entries.filter(|entry| entry.is_ok()).count()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(internal(error)),
        }
    }

    fn read_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<Option<ProviderAttemptRecord>, OrchError> {
        read_json(&self.attempt_path(run_id, attempt_id)?)
    }

    /// Load an attempt and re-check that the caller owns it.
    ///
    /// Ownership is checked rather than the whole scope so a legitimate later
    /// specification revision cannot strand a live record, while a different
    /// account, tenant, installation, agent, run, or repository still can
    /// never address it.
    fn require_attempt(
        &self,
        authority: &ProviderAuthorityScope,
        attempt_id: &str,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        authority.validate()?;
        bounded_field(attempt_id, "attempt.attemptId")?;
        let Some(record) = self.read_attempt(&authority.run_id, attempt_id)? else {
            return deny(
                ProviderAuthorityDenial::AttemptUnknown,
                "provider attempt is unknown to the ledger",
            );
        };
        if record.schema_version != PROVIDER_AUTHORITY_SCHEMA_VERSION {
            return deny(
                ProviderAuthorityDenial::BindingStale,
                "provider attempt record schema version is unsupported",
            );
        }
        let stored = &record.binding.scope;
        if stored.ownership_digest() != authority.ownership_digest() {
            let denial = if stored.owner_principal_id != authority.owner_principal_id
                || stored.tenant_id != authority.tenant_id
                || stored.installation_id != authority.installation_id
            {
                ProviderAuthorityDenial::TenantMismatch
            } else if stored.repository.workspace != authority.repository.workspace {
                ProviderAuthorityDenial::RepositoryMismatch
            } else {
                ProviderAuthorityDenial::BindingMismatch
            };
            return deny(
                denial,
                format!(
                    "provider attempt is owned by a different authority ({})",
                    denial.as_str()
                ),
            );
        }
        Ok(record)
    }

    fn write_attempt(
        &self,
        authority: &ProviderAuthorityScope,
        record: &ProviderAttemptRecord,
    ) -> Result<(), OrchError> {
        let path = self.attempt_path(&authority.run_id, &record.attempt_id)?;
        atomic_write_json(&path, record).map_err(internal)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, OrchError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(internal),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(internal(error)),
    }
}

fn internal(error: impl std::fmt::Display) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, error.to_string())
}

/// Canonical request fingerprint for a provider attempt.
///
/// The fingerprint covers the authority scope, the logical continuation key,
/// and a caller-supplied digest of the exact request payload, plus the attempt
/// ordinal so a legitimate re-issue never collides with the replay guard.
pub fn provider_request_fingerprint(
    authority: &ProviderAuthorityScope,
    continuation_key: &str,
    attempt_ordinal: u32,
    payload_digest: &str,
) -> String {
    hash_payload(&serde_json::json!({
        "schemaVersion": PROVIDER_AUTHORITY_SCHEMA_VERSION,
        "authorityDigest": authority.digest(),
        "continuationKey": continuation_key,
        "attemptOrdinal": attempt_ordinal,
        "payloadDigest": payload_digest,
    }))
}

/// Stable digest for a bounded set of request-shaping inputs.
///
/// Callers pass classification values only: never prompts, never credentials.
pub fn provider_payload_digest(parts: &BTreeMap<String, String>) -> String {
    hash_payload(&serde_json::to_value(parts).unwrap_or(serde_json::Value::Null))
}

// ---------------------------------------------------------------------------
// Authority resolution from durable records
// ---------------------------------------------------------------------------

/// Qualified provider route facts the host resolves before a send.
///
/// These are host-owned: they come from the resolved provider profile and the
/// credential actually loaded, never from a caller payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouteAuthority {
    pub route_class: ProviderRouteClass,
    /// Digest of the normalized endpoint. Use
    /// [`crate::certification::public_xai_endpoint_fingerprint`] for public
    /// xAI routes and an opaque digest for a private compatible gateway.
    pub endpoint_fingerprint: String,
    pub credential_method: CredentialMethodClass,
    /// Digest of the credential identity in use. Never the credential.
    pub credential_binding_digest: String,
}

/// Stable installation identity for a single-tenant desktop host.
///
/// GrokPtah has no multi-tenant service yet, so the durable orchestration
/// store root *is* the installation: it is stable across restarts and distinct
/// between installations. A later service replaces this with its own
/// installation identity without changing the binding shape.
pub fn installation_identity(store_root: &Path) -> String {
    opaque_digest("installation", &store_root.to_string_lossy())
}

/// Tenant identity for a single-tenant host.
///
/// Today the account *is* the tenant. This is a deliberate projection, not an
/// inferred hierarchy: a multi-tenant service supplies a real tenant instead,
/// and every binding, digest, and denial boundary already carries the field.
pub fn single_tenant_identity(owner_principal_id: &str) -> String {
    opaque_digest("tenant", owner_principal_id)
}

/// Durable inputs the host must agree on before an attempt can be bound.
#[derive(Debug, Clone)]
pub struct ProviderAuthorityInputs<'a> {
    pub agent: &'a AgentRecord,
    pub spec: &'a AgentSpec,
    pub run: &'a RunRecord,
    pub route: &'a ProviderRouteAuthority,
    pub tenant_id: &'a str,
    pub installation_id: &'a str,
    /// Repository revision or ref in force for this attempt. When the run is
    /// an isolated execution, this must equal its recorded base revision.
    pub repository_ref: &'a str,
}

/// Digest over the effective authority policy frozen for a specification.
pub fn authority_policy_digest(policy: &AgentAuthorityPolicy) -> String {
    hash_payload(&serde_json::to_value(policy).unwrap_or(serde_json::Value::Null))
}

/// Derive the authority-owned scope for a provider attempt from durable
/// records alone.
///
/// This is the only supported way to obtain a [`ProviderAuthorityScope`] on
/// the production path. It fails closed when the durable records disagree
/// with each other or when the Agent has no claimed account identity, so a
/// legacy or half-migrated record can never be sent on.
pub fn resolve_authority_scope(
    inputs: ProviderAuthorityInputs<'_>,
) -> Result<ProviderAuthorityScope, OrchError> {
    let ProviderAuthorityInputs {
        agent,
        spec,
        run,
        route,
        tenant_id,
        installation_id,
        repository_ref,
    } = inputs;

    let Some(owner_principal_id) = agent.owner_principal_id.as_deref() else {
        return deny(
            ProviderAuthorityDenial::BindingMissing,
            "Agent has no claimed account identity and cannot authorize a provider request",
        );
    };
    if run.agent_id.as_deref() != Some(agent.agent_id.as_str()) {
        return deny(
            ProviderAuthorityDenial::BindingMismatch,
            "run is not owned by this Agent",
        );
    }
    if spec.revision == 0 || run.agent_spec_revision != Some(spec.revision) {
        return deny(
            ProviderAuthorityDenial::BindingStale,
            "run did not freeze this Agent specification revision",
        );
    }
    if !super::workspaces_match(&spec.source_workspace, &run.workspace) {
        return deny(
            ProviderAuthorityDenial::RepositoryMismatch,
            "run workspace does not match the Agent specification workspace",
        );
    }
    if !agent.known_lane_ids().contains(&run.session_id) {
        return deny(
            ProviderAuthorityDenial::BindingMismatch,
            "run Lane is not associated with this Agent",
        );
    }
    if let Some(execution) = run.execution.as_ref() {
        if execution.base_revision != repository_ref {
            return deny(
                ProviderAuthorityDenial::RepositoryMismatch,
                "repository ref does not match the isolated run base revision",
            );
        }
    }

    let scope = ProviderAuthorityScope {
        owner_principal_id: owner_principal_id.to_string(),
        tenant_id: tenant_id.to_string(),
        installation_id: installation_id.to_string(),
        agent_id: agent.agent_id.clone(),
        run_id: run.run_id.clone(),
        lane_id: run.session_id,
        agent_spec_revision: spec.revision,
        model: spec.model.clone(),
        route_class: route.route_class.clone(),
        endpoint_fingerprint: route.endpoint_fingerprint.clone(),
        credential_method: route.credential_method.clone(),
        credential_binding_digest: route.credential_binding_digest.clone(),
        repository: ProviderRepositoryBinding {
            workspace: spec.source_workspace.clone(),
            repository_ref: repository_ref.to_string(),
            policy_digest: authority_policy_digest(&spec.authority),
        },
    };
    scope.validate()?;
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certification::scan_value_for_forbidden_data;

    /// One named edit to an authority scope, used to drive table tests.
    type ScopeMutation = Box<dyn Fn(&mut ProviderAuthorityScope)>;

    fn scope() -> ProviderAuthorityScope {
        ProviderAuthorityScope {
            owner_principal_id: "account-alpha".into(),
            tenant_id: "tenant-alpha".into(),
            installation_id: "installation-1".into(),
            agent_id: "agent-1".into(),
            run_id: "run-1".into(),
            lane_id: Uuid::from_u128(7),
            agent_spec_revision: 3,
            model: AgentModelSpec::from_selection_key("grok-4-fast").unwrap(),
            route_class: ProviderRouteClass::GrokBuildProxy,
            endpoint_fingerprint: "a".repeat(64),
            credential_method: CredentialMethodClass::GrokBuildOidc,
            credential_binding_digest: "b".repeat(64),
            repository: ProviderRepositoryBinding {
                workspace: "/srv/workspaces/alpha".into(),
                repository_ref: "refs/heads/main".into(),
                policy_digest: "c".repeat(64),
            },
        }
    }

    fn binding(scope: ProviderAuthorityScope, now: DateTime<Utc>) -> ProviderAuthorityBinding {
        let fingerprint = provider_request_fingerprint(&scope, "round-1", 1, &"d".repeat(64));
        ProviderAuthorityBinding::bind(
            scope,
            fingerprint,
            "round-1",
            now,
            Duration::milliseconds(DEFAULT_BINDING_TTL_MS),
        )
        .unwrap()
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000, 0).unwrap()
    }

    #[test]
    fn send_state_lattice_permits_only_honest_transitions() {
        use ProviderSendState::*;
        let legal = [
            (KnownNotSent, Sending),
            (KnownNotSent, Settled),
            (Sending, Settled),
            (Sending, Uncertain),
            (Uncertain, Settled),
        ];
        for from in [KnownNotSent, Sending, Uncertain, Settled] {
            for to in [KnownNotSent, Sending, Uncertain, Settled] {
                assert_eq!(
                    from.can_transition_to(to),
                    legal.contains(&(from, to)),
                    "unexpected legality for {} -> {}",
                    from.as_str(),
                    to.as_str()
                );
            }
        }
        // An uncertain attempt can never be resurrected into another send.
        assert!(!Uncertain.can_transition_to(Sending));
        assert!(!Settled.can_transition_to(Sending));
    }

    #[test]
    fn only_a_known_unsent_attempt_may_be_auto_retried() {
        assert!(ProviderSendState::KnownNotSent.auto_retry_allowed());
        for state in [
            ProviderSendState::Sending,
            ProviderSendState::Uncertain,
            ProviderSendState::Settled,
        ] {
            assert!(
                !state.auto_retry_allowed(),
                "{} must never be auto-retried",
                state.as_str()
            );
        }
    }

    #[test]
    fn binding_requires_every_authority_field() {
        let base = scope();
        let mutations: Vec<(&str, ScopeMutation)> = vec![
            (
                "ownerPrincipalId",
                Box::new(|s: &mut ProviderAuthorityScope| s.owner_principal_id = String::new()),
            ),
            (
                "tenantId",
                Box::new(|s: &mut ProviderAuthorityScope| s.tenant_id = "   ".into()),
            ),
            (
                "installationId",
                Box::new(|s: &mut ProviderAuthorityScope| s.installation_id = String::new()),
            ),
            (
                "agentId",
                Box::new(|s: &mut ProviderAuthorityScope| s.agent_id = String::new()),
            ),
            (
                "runId",
                Box::new(|s: &mut ProviderAuthorityScope| s.run_id = String::new()),
            ),
            (
                "agentSpecRevision",
                Box::new(|s: &mut ProviderAuthorityScope| s.agent_spec_revision = 0),
            ),
            (
                "endpointFingerprint",
                Box::new(|s: &mut ProviderAuthorityScope| s.endpoint_fingerprint = String::new()),
            ),
            (
                "credentialBindingDigest",
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.credential_binding_digest = String::new()
                }),
            ),
            (
                "repository.workspace",
                Box::new(|s: &mut ProviderAuthorityScope| s.repository.workspace = String::new()),
            ),
            (
                "repository.repositoryRef",
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.repository.repository_ref = String::new()
                }),
            ),
            (
                "repository.policyDigest",
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.repository.policy_digest = String::new()
                }),
            ),
        ];
        for (field, mutate) in mutations {
            let mut broken = base.clone();
            mutate(&mut broken);
            let error = broken.validate().expect_err(field);
            assert_eq!(
                error.data,
                Some(serde_json::json!({
                    "denial": ProviderAuthorityDenial::BindingMissing.as_str()
                })),
                "missing {field} must fail closed as binding_missing"
            );
        }
    }

    #[test]
    fn binding_lifetime_must_be_positive() {
        let at = now();
        let mut binding = binding(scope(), at);
        binding.expires_at = binding.bound_at;
        assert_eq!(
            binding.validate().unwrap_err().data,
            Some(serde_json::json!({ "denial": "binding_missing" }))
        );
    }

    #[test]
    fn authority_mismatch_denies_on_the_exact_boundary() {
        let at = now();
        let authority = scope();
        let cases: Vec<(ProviderAuthorityDenial, ScopeMutation)> = vec![
            (
                ProviderAuthorityDenial::TenantMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| s.tenant_id = "tenant-beta".into()),
            ),
            (
                ProviderAuthorityDenial::TenantMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.owner_principal_id = "account-beta".into()
                }),
            ),
            (
                ProviderAuthorityDenial::TenantMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.installation_id = "installation-2".into()
                }),
            ),
            (
                ProviderAuthorityDenial::RepositoryMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.repository.workspace = "/srv/workspaces/beta".into()
                }),
            ),
            (
                ProviderAuthorityDenial::RepositoryMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.repository.repository_ref = "refs/heads/release".into()
                }),
            ),
            (
                ProviderAuthorityDenial::RepositoryMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.repository.policy_digest = "e".repeat(64)
                }),
            ),
            (
                ProviderAuthorityDenial::RouteNotAuthorized,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.route_class = ProviderRouteClass::CompatibleGateway
                }),
            ),
            (
                ProviderAuthorityDenial::RouteNotAuthorized,
                Box::new(|s: &mut ProviderAuthorityScope| s.endpoint_fingerprint = "f".repeat(64)),
            ),
            (
                ProviderAuthorityDenial::RouteNotAuthorized,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.credential_method = CredentialMethodClass::ApiKeyReference
                }),
            ),
            (
                ProviderAuthorityDenial::RouteNotAuthorized,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.credential_binding_digest = "0".repeat(64)
                }),
            ),
            (
                ProviderAuthorityDenial::ModelMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| {
                    s.model = AgentModelSpec::from_selection_key("grok-4-heavy").unwrap()
                }),
            ),
            (
                ProviderAuthorityDenial::BindingStale,
                Box::new(|s: &mut ProviderAuthorityScope| s.agent_spec_revision = 4),
            ),
            (
                ProviderAuthorityDenial::BindingMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| s.agent_id = "agent-2".into()),
            ),
            (
                ProviderAuthorityDenial::BindingMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| s.run_id = "run-2".into()),
            ),
            (
                ProviderAuthorityDenial::BindingMismatch,
                Box::new(|s: &mut ProviderAuthorityScope| s.lane_id = Uuid::from_u128(9)),
            ),
        ];
        for (expected, mutate) in cases {
            let mut claimed = authority.clone();
            mutate(&mut claimed);
            let binding = binding(claimed, at);
            let error = binding
                .authorize_start(&authority, at)
                .expect_err(expected.as_str());
            assert_eq!(
                error.data,
                Some(serde_json::json!({ "denial": expected.as_str() })),
                "expected {} denial",
                expected.as_str()
            );
            assert_eq!(error.code, expected.error_code());
        }
    }

    #[test]
    fn expired_and_premature_bindings_are_stale() {
        let at = now();
        let authority = scope();
        let binding = binding(authority.clone(), at);
        assert!(binding.authorize_start(&authority, at).is_ok());
        let expired = binding
            .authorize_start(&authority, binding.expires_at)
            .unwrap_err();
        assert_eq!(
            expired.data,
            Some(serde_json::json!({ "denial": "binding_stale" }))
        );
        assert_eq!(expired.code, OrchErrorCode::StaleVersion);
        let premature = binding
            .authorize_start(&authority, at - Duration::seconds(1))
            .unwrap_err();
        assert_eq!(
            premature.data,
            Some(serde_json::json!({ "denial": "binding_stale" }))
        );
    }

    #[test]
    fn binding_digest_covers_every_authority_component() {
        let at = now();
        let base = binding(scope(), at).binding_digest();
        let mutations: Vec<ScopeMutation> = vec![
            Box::new(|s: &mut ProviderAuthorityScope| s.tenant_id = "tenant-beta".into()),
            Box::new(|s: &mut ProviderAuthorityScope| s.installation_id = "installation-2".into()),
            Box::new(|s: &mut ProviderAuthorityScope| s.agent_spec_revision = 4),
            Box::new(|s: &mut ProviderAuthorityScope| s.route_class = ProviderRouteClass::XaiApi),
            Box::new(|s: &mut ProviderAuthorityScope| {
                s.repository.repository_ref = "refs/heads/next".into()
            }),
            Box::new(|s: &mut ProviderAuthorityScope| {
                s.model = AgentModelSpec::from_selection_key("grok-4-heavy").unwrap()
            }),
        ];
        for mutate in mutations {
            let mut changed = scope();
            mutate(&mut changed);
            assert_ne!(base, binding(changed, at).binding_digest());
        }
    }

    fn record_in(state: ProviderSendState) -> ProviderAttemptRecord {
        let at = now();
        ProviderAttemptRecord {
            schema_version: PROVIDER_AUTHORITY_SCHEMA_VERSION,
            attempt_id: "attempt-1".into(),
            binding: binding(scope(), at),
            request: ProviderRequestIdentity::new("client-request-1"),
            intent: ProviderContinuationIntent::default(),
            send_state: state,
            uncertainty_reason: None,
            settled_outcome: None,
            authorizing_grant_id: None,
            transitions: Vec::new(),
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn unknowns_are_explicit_for_every_send_state() {
        let not_sent = record_in(ProviderSendState::KnownNotSent);
        assert_eq!(
            not_sent.unknowns(),
            vec![
                ProviderUnknown::ProviderRequestId,
                ProviderUnknown::ProviderOutcome,
                ProviderUnknown::Usage,
            ]
        );
        // Delivery is knowable before transport, so it is not an unknown.
        assert!(!not_sent.unknowns().contains(&ProviderUnknown::Delivery));

        for state in [ProviderSendState::Sending, ProviderSendState::Uncertain] {
            let record = record_in(state);
            assert!(
                record.unknowns().contains(&ProviderUnknown::Delivery),
                "{} must report delivery as unknown",
                state.as_str()
            );
        }

        let mut settled = record_in(ProviderSendState::Settled);
        settled.settled_outcome = Some(ProviderSettledOutcome::Delivered);
        settled.request.provider_request_id = Some("provider-request-1".into());
        assert!(settled.unknowns().is_empty());

        let mut abandoned = record_in(ProviderSendState::Settled);
        abandoned.settled_outcome = Some(ProviderSettledOutcome::AbandonedBeforeSend);
        abandoned.request.provider_request_id = Some("provider-request-1".into());
        assert_eq!(abandoned.unknowns(), vec![ProviderUnknown::Usage]);
    }

    #[test]
    fn receipt_is_bounded_and_secret_free() {
        let mut record = record_in(ProviderSendState::Sending);
        record.request.provider_request_id = Some("provider-request-1".into());
        record.authorizing_grant_id = Some(Uuid::from_u128(11).to_string());
        let receipt = record.receipt();
        assert_eq!(receipt.schema, PROVIDER_ATTEMPT_RECEIPT_SCHEMA);
        assert_eq!(receipt.send_state, ProviderSendState::Sending);
        assert!(!receipt.auto_retry_allowed);
        assert!(receipt.confirmed);

        let value = serde_json::to_value(&receipt).unwrap();
        scan_value_for_forbidden_data(&value)
            .expect("provider attempt receipt must be secret-free");

        let text = serde_json::to_string(&value).unwrap();
        // Raw account, tenant, installation, and workspace values never appear.
        for raw in [
            "account-alpha",
            "tenant-alpha",
            "installation-1",
            "/srv/workspaces/alpha",
        ] {
            assert!(!text.contains(raw), "receipt leaked {raw}");
        }
        // Their digests do, so an operator can still compare bindings.
        assert_eq!(
            receipt.authority.tenant_digest,
            opaque_digest("tenant", "tenant-alpha")
        );
        assert_eq!(
            receipt.authority.workspace_digest,
            opaque_digest("workspace", "/srv/workspaces/alpha")
        );
    }

    #[test]
    fn nonce_digest_never_reveals_the_nonce() {
        let nonce = new_confirmation_nonce();
        assert!(nonce.len() >= MIN_CONFIRMATION_NONCE_BYTES);
        let digest = confirmation_nonce_digest(&nonce);
        assert_eq!(digest.len(), 64);
        assert!(!digest.contains(&nonce));
        assert_ne!(digest, confirmation_nonce_digest(&new_confirmation_nonce()));
        assert_eq!(digest, confirmation_nonce_digest(&nonce));
    }

    #[test]
    fn denials_map_onto_the_existing_error_contract() {
        for (denial, code) in [
            (
                ProviderAuthorityDenial::BindingMissing,
                OrchErrorCode::InvalidRequest,
            ),
            (
                ProviderAuthorityDenial::BindingStale,
                OrchErrorCode::StaleVersion,
            ),
            (
                ProviderAuthorityDenial::BindingReplayed,
                OrchErrorCode::Conflict,
            ),
            (
                ProviderAuthorityDenial::UncertainAttemptNotRetryable,
                OrchErrorCode::Conflict,
            ),
            (
                ProviderAuthorityDenial::RepositoryMismatch,
                OrchErrorCode::WorkspaceMismatch,
            ),
            (
                ProviderAuthorityDenial::TenantMismatch,
                OrchErrorCode::ForbiddenScope,
            ),
            (
                ProviderAuthorityDenial::GrantAlreadyConsumed,
                OrchErrorCode::Conflict,
            ),
            (
                ProviderAuthorityDenial::LedgerBoundExceeded,
                OrchErrorCode::CapacityExhausted,
            ),
        ] {
            let error = denial.into_error("boundary refused");
            assert_eq!(error.code, code, "{}", denial.as_str());
            assert_eq!(
                error.data,
                Some(serde_json::json!({ "denial": denial.as_str() }))
            );
        }
    }

    #[test]
    fn cancellation_intent_and_timestamp_must_agree() {
        let mut intent = ProviderContinuationIntent {
            follow_up: FollowUpDisposition::ContinueRun,
            cancel: CancelDisposition::Requested,
            ..ProviderContinuationIntent::default()
        };
        assert!(intent.validate().is_err());
        intent.cancel_requested_at = Some(now());
        assert!(intent.validate().is_ok());
        intent.cancel = CancelDisposition::NotRequested;
        assert!(intent.validate().is_err());
    }

    #[test]
    fn request_fingerprint_separates_ordinals_and_authorities() {
        let authority = scope();
        let payload = "d".repeat(64);
        let first = provider_request_fingerprint(&authority, "round-1", 1, &payload);
        assert_ne!(
            first,
            provider_request_fingerprint(&authority, "round-1", 2, &payload)
        );
        assert_ne!(
            first,
            provider_request_fingerprint(&authority, "round-2", 1, &payload)
        );
        let mut other_tenant = authority.clone();
        other_tenant.tenant_id = "tenant-beta".into();
        assert_ne!(
            first,
            provider_request_fingerprint(&other_tenant, "round-1", 1, &payload)
        );
        assert_eq!(
            first,
            provider_request_fingerprint(&authority, "round-1", 1, &payload)
        );
    }

    #[test]
    fn payload_digest_is_order_independent_and_content_sensitive() {
        let mut left = BTreeMap::new();
        left.insert("effort".to_string(), "high".to_string());
        left.insert("toolCount".to_string(), "12".to_string());
        let mut right = BTreeMap::new();
        right.insert("toolCount".to_string(), "12".to_string());
        right.insert("effort".to_string(), "high".to_string());
        assert_eq!(
            provider_payload_digest(&left),
            provider_payload_digest(&right)
        );
        right.insert("effort".to_string(), "low".to_string());
        assert_ne!(
            provider_payload_digest(&left),
            provider_payload_digest(&right)
        );
    }
    // -- authority resolution from durable records -----------------------

    fn agent_record(workspace: &str, lane: Uuid, spec: AgentSpec) -> AgentRecord {
        AgentRecord {
            agent_id: "agent-1".into(),
            owner_principal_id: Some("account-alpha".into()),
            session_id: lane,
            lane_ids: vec![lane],
            lane_associations: Vec::new(),
            workspace: workspace.into(),
            model: spec.model.selection_key.clone(),
            spec: Some(spec),
            state: super::super::types::AgentState::Active,
            current_run_id: Some("run-1".into()),
            last_run_id: None,
            last_lane_id: Some(lane),
            latest_checkpoint_id: None,
            continuation_ordinal: 1,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn agent_spec(workspace: &str) -> AgentSpec {
        let mut spec = AgentSpec::initial(
            "agent-1",
            workspace,
            "grok-4-fast",
            AgentAuthorityPolicy::default(),
            now(),
            "test",
        )
        .unwrap();
        spec.revision = 3;
        spec.previous_revision = Some(2);
        spec.validate().unwrap();
        spec
    }

    fn run_record(workspace: &str, lane: Uuid) -> RunRecord {
        RunRecord {
            run_id: "run-1".into(),
            session_id: lane,
            workspace: workspace.into(),
            request_id: "request-1".into(),
            client_id: None,
            state: super::super::types::RunState::Running,
            purpose: Default::default(),
            agent_id: Some("agent-1".into()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: Some(3),
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: Default::default(),
            prompt_preview: "work".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now(),
            updated_at: now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    fn route() -> ProviderRouteAuthority {
        ProviderRouteAuthority {
            route_class: ProviderRouteClass::GrokBuildProxy,
            endpoint_fingerprint: "a".repeat(64),
            credential_method: CredentialMethodClass::GrokBuildOidc,
            credential_binding_digest: "b".repeat(64),
        }
    }

    fn resolve_with(
        agent: &AgentRecord,
        spec: &AgentSpec,
        run: &RunRecord,
    ) -> Result<ProviderAuthorityScope, OrchError> {
        resolve_authority_scope(ProviderAuthorityInputs {
            agent,
            spec,
            run,
            route: &route(),
            tenant_id: "tenant-alpha",
            installation_id: "installation-1",
            repository_ref: "refs/heads/main",
        })
    }

    #[test]
    fn authority_scope_is_derived_from_durable_records() {
        let workspace = std::env::temp_dir().to_string_lossy().to_string();
        let lane = Uuid::from_u128(7);
        let spec = agent_spec(&workspace);
        let agent = agent_record(&workspace, lane, spec.clone());
        let run = run_record(&workspace, lane);

        let resolved = resolve_with(&agent, &spec, &run).unwrap();
        assert_eq!(resolved.owner_principal_id, "account-alpha");
        assert_eq!(resolved.agent_spec_revision, 3);
        assert_eq!(resolved.lane_id, lane);
        assert_eq!(resolved.model, spec.model);
        assert_eq!(
            resolved.repository.policy_digest,
            authority_policy_digest(&spec.authority)
        );
        // A binding built from the derived scope authorizes against itself.
        let binding = binding(resolved.clone(), now());
        assert!(binding.authorize_start(&resolved, now()).is_ok());
    }

    #[test]
    fn authority_resolution_fails_closed_on_disagreeing_records() {
        let workspace = std::env::temp_dir().to_string_lossy().to_string();
        let lane = Uuid::from_u128(7);
        let spec = agent_spec(&workspace);
        let agent = agent_record(&workspace, lane, spec.clone());
        let run = run_record(&workspace, lane);

        // Unclaimed account identity.
        let mut legacy = agent.clone();
        legacy.owner_principal_id = None;
        assert_eq!(
            resolve_with(&legacy, &spec, &run).unwrap_err().data,
            Some(serde_json::json!({ "denial": "binding_missing" }))
        );

        // Run owned by another Agent.
        let mut foreign = run.clone();
        foreign.agent_id = Some("agent-2".into());
        assert_eq!(
            resolve_with(&agent, &spec, &foreign).unwrap_err().data,
            Some(serde_json::json!({ "denial": "binding_mismatch" }))
        );

        // Run never froze a specification revision.
        let mut unfrozen = run.clone();
        unfrozen.agent_spec_revision = None;
        assert_eq!(
            resolve_with(&agent, &spec, &unfrozen).unwrap_err().data,
            Some(serde_json::json!({ "denial": "binding_stale" }))
        );

        // Run froze a different revision than the specification presented.
        let mut superseded = run.clone();
        superseded.agent_spec_revision = Some(2);
        assert_eq!(
            resolve_with(&agent, &spec, &superseded).unwrap_err().data,
            Some(serde_json::json!({ "denial": "binding_stale" }))
        );

        // Run workspace disagrees with the specification workspace.
        let mut elsewhere = run.clone();
        elsewhere.workspace = "/srv/workspaces/beta".into();
        assert_eq!(
            resolve_with(&agent, &spec, &elsewhere).unwrap_err().data,
            Some(serde_json::json!({ "denial": "repository_mismatch" }))
        );

        // Lane is not associated with the Agent.
        let mut other_lane = run.clone();
        other_lane.session_id = Uuid::from_u128(99);
        assert_eq!(
            resolve_with(&agent, &spec, &other_lane).unwrap_err().data,
            Some(serde_json::json!({ "denial": "binding_mismatch" }))
        );
    }

    #[test]
    fn an_isolated_run_pins_its_base_revision() {
        let workspace = std::env::temp_dir().to_string_lossy().to_string();
        let lane = Uuid::from_u128(7);
        let spec = agent_spec(&workspace);
        let agent = agent_record(&workspace, lane, spec.clone());
        let mut run = run_record(&workspace, lane);
        run.execution = Some(super::super::types::RunExecution {
            mode: super::super::types::RunExecutionMode::IsolatedWorktree,
            source_workspace: workspace.clone(),
            execution_workspace: workspace.clone(),
            base_revision: "refs/heads/release".into(),
            source_fingerprint: "1".repeat(64),
            final_fingerprint: None,
            promotion_state: Default::default(),
            promoted_at: None,
        });
        assert_eq!(
            resolve_with(&agent, &spec, &run).unwrap_err().data,
            Some(serde_json::json!({ "denial": "repository_mismatch" }))
        );

        let resolved = resolve_authority_scope(ProviderAuthorityInputs {
            agent: &agent,
            spec: &spec,
            run: &run,
            route: &route(),
            tenant_id: "tenant-alpha",
            installation_id: "installation-1",
            repository_ref: "refs/heads/release",
        })
        .unwrap();
        assert_eq!(resolved.repository.repository_ref, "refs/heads/release");
    }

    #[test]
    fn policy_digest_moves_with_the_effective_authority() {
        let mut policy = AgentAuthorityPolicy::default();
        let base = authority_policy_digest(&policy);
        policy.bypass_permissions = true;
        assert_ne!(base, authority_policy_digest(&policy));
        policy.bypass_permissions = false;
        assert_eq!(base, authority_policy_digest(&policy));
        policy.deny_rules.push("Write(/etc/**)".into());
        assert_ne!(base, authority_policy_digest(&policy));
    }

    #[test]
    fn installation_and_tenant_identities_are_stable_and_distinct() {
        let left = installation_identity(Path::new("/srv/state/one"));
        let right = installation_identity(Path::new("/srv/state/two"));
        assert_ne!(left, right);
        assert_eq!(left, installation_identity(Path::new("/srv/state/one")));
        assert_eq!(left.len(), 64);
        assert_ne!(
            single_tenant_identity("account-alpha"),
            single_tenant_identity("account-beta")
        );
    }
}
