//! Host-issued Computer Use approval receipts (`computer.control` human gate).
//!
//! `computer.control` is advertised with `human_gate: true`. Advertising a
//! gate is not enforcing one: transport authentication proves *who is
//! calling*, and an initialized MCP session proves *that a tool is
//! reachable*. Neither proves that a human approved this exact control
//! request. This module is the authority that does.
//!
//! The separation is deliberate and one-directional:
//!
//! * **Requesting** an approval ([`ApprovalRecord::request`]) is something an
//!   agent may do. It creates a `Pending` record that carries **no** authority
//!   and cannot be self-promoted.
//! * **Issuing** a receipt ([`ApprovalRecord::approve`]) is something only the
//!   trusted host does, on an explicit local-operator decision.
//! * **Consuming** a receipt ([`ApprovalRecord::check_consumable`]) happens
//!   exactly once, server-side, immediately before control authority is
//!   attached to a run.
//!
//! A receipt is bound to every dimension that could otherwise be swapped
//! underneath it: control-plane principal and bearer-token fingerprint, MCP
//! transport session, client actor, workspace, owning session, run identity
//! **and run revision**, the exact approved action classes and bounds, the
//! capability-contract revision that was advertised when the human decided,
//! and a one-time nonce. Every one of those is re-validated at consumption;
//! none of them is accepted from a caller-supplied Boolean.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::control::ComputerGrantRequest;
use super::types::{
    validate_id, validate_workspace, ActionClass, ComputerError, ComputerErrorCode, ComputerResult,
    ComputerUseLimits,
};

/// Versioned identifier for the durable approval-receipt record shape.
pub const APPROVAL_CONTRACT_VERSION: &str = "grokptah.computer.approval.v1";

/// The one capability whose human gate this receipt satisfies. A receipt is
/// never generic authority: it names the capability it was minted for.
pub const COMPUTER_CONTROL_CAPABILITY: &str = "computer.control";

/// How long a `Pending` request stays answerable before it fails closed.
pub const APPROVAL_REQUEST_TTL: Duration = Duration::minutes(10);

/// How long an issued receipt stays consumable after the human decides.
///
/// This is the window for *presenting* the receipt, deliberately separate
/// from the action lease TTL the receipt authorizes.
pub const APPROVAL_RECEIPT_TTL: Duration = Duration::minutes(5);

/// Bytes of entropy in the one-time nonce handed to the requester.
const NONCE_BYTES: usize = 32;

/// Control-plane identity a receipt is minted for.
///
/// `token_fingerprint` is a digest of the presented bearer token, not the
/// token: rotating the control token invalidates every outstanding receipt
/// without the ledger ever storing the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPrincipal {
    /// Control-plane token identity (`AuthContext::token_id`).
    pub principal_id: String,
    /// Lowercase hex SHA-256 of the presented bearer token.
    pub token_fingerprint: String,
    /// Server-issued MCP transport session id.
    pub mcp_session_id: String,
    /// `name@version#transport-session` actor identity.
    pub client_actor_id: String,
}

impl ApprovalPrincipal {
    /// Shape-validate a principal before it is bound into a durable record.
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("principal_id", &self.principal_id)?;
        validate_id("mcp_session_id", &self.mcp_session_id)?;
        validate_id("client_actor_id", &self.client_actor_id)?;
        validate_fingerprint("token_fingerprint", &self.token_fingerprint)?;
        Ok(())
    }
}

/// Exact run identity, including the revision the human saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalScope {
    /// Owning GrokPtah session.
    pub owner_session_id: Uuid,
    /// Canonical workspace string bound to the run.
    pub workspace: String,
    /// Durable Computer Run identity.
    pub run_id: String,
    /// Run revision at approval time. A run that moves invalidates the receipt.
    pub run_version: u64,
}

impl ApprovalScope {
    /// Shape-validate a scope before it is bound into a durable record.
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("run_id", &self.run_id)?;
        validate_workspace(Some(&self.workspace))?;
        if self.run_version == 0 {
            return Err(invalid("approval scope requires a run revision"));
        }
        Ok(())
    }
}

/// The exact authority a human approved. Consumption may narrow it; nothing
/// may widen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalBounds {
    /// Action classes the human approved. `KeyChord` and `PointerFallback`
    /// are rejected here as they are on the grant path.
    pub action_classes: BTreeSet<ActionClass>,
    /// Maximum actions the resulting lease may permit.
    pub max_uses: u32,
    /// Maximum lease lifetime the resulting grant may carry.
    pub max_ttl_ms: u64,
}

impl ApprovalBounds {
    /// Validate the approved bounds against the run's own ceilings.
    pub fn validate(&self, limits: ComputerUseLimits) -> ComputerResult<()> {
        if self.action_classes.is_empty()
            || self.max_uses == 0
            || self.max_uses > limits.max_actions
            || self.max_ttl_ms == 0
            || self.max_ttl_ms > limits.max_duration_secs.saturating_mul(1_000)
            || self
                .action_classes
                .iter()
                .any(|class| matches!(class, ActionClass::KeyChord | ActionClass::PointerFallback))
        {
            return Err(invalid("invalid computer use approval bounds"));
        }
        Ok(())
    }

    /// Whether `request` stays inside these bounds.
    ///
    /// An unbounded (`None`) use count is treated as over-broad: a human
    /// approves a finite number of actions, never "as many as you like".
    pub fn covers(&self, request: &ComputerGrantRequest) -> bool {
        request
            .action_classes
            .iter()
            .all(|class| self.action_classes.contains(class))
            && request.ttl_ms <= self.max_ttl_ms
            && request
                .uses_remaining
                .is_some_and(|uses| uses > 0 && uses <= self.max_uses)
    }
}

/// Every dimension a receipt is bound to, recomputed live at consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalBinding {
    /// Capability whose human gate this receipt satisfies.
    pub capability_id: String,
    /// Digest of the capability set advertised when the human decided.
    pub capability_revision: String,
    /// Caller identity.
    pub principal: ApprovalPrincipal,
    /// Run and workspace identity.
    pub scope: ApprovalScope,
    /// Approved action authority.
    pub bounds: ApprovalBounds,
}

impl ApprovalBinding {
    /// Shape-validate the full binding.
    pub fn validate(&self, limits: ComputerUseLimits) -> ComputerResult<()> {
        if self.capability_id != COMPUTER_CONTROL_CAPABILITY {
            return Err(invalid("unsupported computer use approval capability"));
        }
        validate_fingerprint("capability_revision", &self.capability_revision)?;
        self.principal.validate()?;
        self.scope.validate()?;
        self.bounds.validate(limits)?;
        Ok(())
    }

    /// Exact equality over every identity dimension.
    ///
    /// Two things are deliberately excluded. Bounds are checked by
    /// [`ApprovalBounds::covers`], which permits narrowing. `run_version` is
    /// a freshness fence rather than an identity: a receipt presented against
    /// the right run at the wrong revision is *stale*, and saying so is a
    /// useful diagnostic for the legitimate requester — who has already
    /// proven possession of the nonce — rather than an oracle. Identity
    /// itself never narrows: it matches exactly or the receipt is unusable.
    fn identity_matches(&self, other: &Self) -> bool {
        self.capability_id == other.capability_id
            && self.principal == other.principal
            && self.scope.owner_session_id == other.scope.owner_session_id
            && self.scope.workspace == other.scope.workspace
            && self.scope.run_id == other.scope.run_id
    }
}

/// Durable lifecycle of one approval record.
///
/// `Expired` is derived rather than stored so a clock change can never
/// resurrect an expired receipt by rewriting a stored state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// Requested by an agent; carries no authority.
    Pending,
    /// Issued by the trusted host after an explicit human decision.
    Approved,
    /// Refused by the human.
    Denied,
    /// Consumed exactly once by the server-side control path.
    Consumed,
    /// Invalidated by the host (operator revoke, takeover, or restart).
    Revoked,
}

impl ApprovalState {
    /// Whether no further transition is possible.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Denied | Self::Consumed | Self::Revoked)
    }
}

/// Time-resolved status projected to callers and operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    /// Awaiting a human decision.
    Pending,
    /// Issued and still consumable.
    Approved,
    /// Refused by the human.
    Denied,
    /// Already consumed; a second use is a replay.
    Consumed,
    /// Invalidated by the host.
    Revoked,
    /// Timed out before it was answered or before it was consumed.
    Expired,
}

/// What a caller presents to consume a receipt.
///
/// The nonce is issued exactly once, to the requester, over the same
/// authenticated channel that created the request. The ledger stores only its
/// digest, so a stolen ledger yields no usable receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPresentation {
    /// Identifier of the approval record.
    pub approval_id: String,
    /// One-time secret returned when the request was created.
    pub nonce: String,
}

impl ApprovalPresentation {
    /// Shape-validate a presentation before any ledger lookup.
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("approval_id", &self.approval_id)?;
        if self.nonce.len() != NONCE_BYTES * 2
            || !self.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(not_available());
        }
        Ok(())
    }
}

/// The durable approval record. This *is* the receipt: it is host-issued,
/// versioned, fully bound, and consumable exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRecord {
    /// Record contract version.
    pub contract: String,
    /// Stable record identity.
    pub approval_id: String,
    /// Idempotency key of the request that created this record.
    pub request_id: String,
    /// Everything the receipt is bound to.
    pub binding: ApprovalBinding,
    /// Durable lifecycle state.
    pub state: ApprovalState,
    /// Lowercase hex SHA-256 of the one-time nonce.
    pub nonce_hash: String,
    /// When the agent asked.
    pub requested_at: DateTime<Utc>,
    /// When the unanswered request stops being answerable.
    pub request_expires_at: DateTime<Utc>,
    /// When the human decided.
    pub issued_at: Option<DateTime<Utc>>,
    /// When an issued receipt stops being consumable.
    pub expires_at: Option<DateTime<Utc>>,
    /// When the server consumed it.
    pub consumed_at: Option<DateTime<Utc>>,
    /// Mutation request id that consumed it, for audit correlation.
    pub consumed_by_request_id: Option<String>,
}

/// A freshly created request plus the one-time nonce, returned to the
/// requester exactly once and never persisted in the clear.
#[derive(Debug, Clone)]
pub struct IssuedApprovalRequest {
    /// The durable record.
    pub record: ApprovalRecord,
    /// The one-time secret. Hand to the requester; never store or log.
    pub nonce: String,
}

impl ApprovalRecord {
    /// Create a `Pending` request. This grants nothing.
    pub fn request(
        request_id: &str,
        binding: ApprovalBinding,
        limits: ComputerUseLimits,
        now: DateTime<Utc>,
    ) -> ComputerResult<IssuedApprovalRequest> {
        validate_id("request_id", request_id)?;
        binding.validate(limits)?;
        let nonce = fresh_nonce();
        Ok(IssuedApprovalRequest {
            record: Self {
                contract: APPROVAL_CONTRACT_VERSION.into(),
                approval_id: Uuid::new_v4().to_string(),
                request_id: request_id.into(),
                binding,
                state: ApprovalState::Pending,
                nonce_hash: digest_hex(nonce.as_bytes()),
                requested_at: now,
                request_expires_at: now + APPROVAL_REQUEST_TTL,
                issued_at: None,
                expires_at: None,
                consumed_at: None,
                consumed_by_request_id: None,
            },
            nonce,
        })
    }

    /// Time-resolved status.
    pub fn status_at(&self, now: DateTime<Utc>) -> ApprovalStatus {
        match self.state {
            ApprovalState::Pending if now >= self.request_expires_at => ApprovalStatus::Expired,
            ApprovalState::Pending => ApprovalStatus::Pending,
            ApprovalState::Approved if self.expires_at.is_none_or(|expires| now >= expires) => {
                ApprovalStatus::Expired
            }
            ApprovalState::Approved => ApprovalStatus::Approved,
            ApprovalState::Denied => ApprovalStatus::Denied,
            ApprovalState::Consumed => ApprovalStatus::Consumed,
            ApprovalState::Revoked => ApprovalStatus::Revoked,
        }
    }

    /// Host-side issuance after an explicit human decision.
    ///
    /// `capability_revision` is re-read at decision time so the receipt
    /// records the contract the human actually saw, not the one advertised
    /// when the agent asked.
    pub fn approve(&mut self, capability_revision: &str, now: DateTime<Utc>) -> ComputerResult<()> {
        validate_fingerprint("capability_revision", capability_revision)?;
        if self.status_at(now) != ApprovalStatus::Pending {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer use approval is no longer answerable",
            ));
        }
        self.binding.capability_revision = capability_revision.to_owned();
        self.state = ApprovalState::Approved;
        self.issued_at = Some(now);
        self.expires_at = Some(now + APPROVAL_RECEIPT_TTL);
        Ok(())
    }

    /// Host-side refusal.
    pub fn deny(&mut self, now: DateTime<Utc>) -> ComputerResult<()> {
        if self.status_at(now) != ApprovalStatus::Pending {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer use approval is no longer answerable",
            ));
        }
        self.state = ApprovalState::Denied;
        self.issued_at = Some(now);
        Ok(())
    }

    /// Host-side invalidation of an un-consumed record.
    ///
    /// Consumed records are left alone: their terminal state is the durable
    /// anti-replay fact and must never be rewritten.
    pub fn revoke(&mut self) -> bool {
        if matches!(self.state, ApprovalState::Pending | ApprovalState::Approved) {
            self.state = ApprovalState::Revoked;
            return true;
        }
        false
    }

    /// Verify a presentation against this record without mutating it.
    ///
    /// Ordering is a security property. The nonce is checked first, in
    /// constant time, and every failure up to and including binding equality
    /// returns one indistinguishable error — an unauthorized caller learns
    /// nothing about whether an approval id exists, who owns it, or what it
    /// covers. Only once the caller has proven possession of the nonce do
    /// state, expiry, replay, revision, and narrowing failures become
    /// distinguishable, because at that point they are diagnostics for the
    /// legitimate requester rather than an oracle.
    pub fn check_consumable(
        &self,
        presentation: &ApprovalPresentation,
        live: &ApprovalBinding,
        requested: &ComputerGrantRequest,
        live_run_version: u64,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        presentation.validate()?;
        if presentation.approval_id != self.approval_id
            || !constant_time_hex_eq(&digest_hex(presentation.nonce.as_bytes()), &self.nonce_hash)
            || !self.binding.identity_matches(live)
        {
            return Err(not_available());
        }
        match self.status_at(now) {
            ApprovalStatus::Approved => {}
            ApprovalStatus::Consumed => {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "computer use approval receipt was already consumed",
                ));
            }
            ApprovalStatus::Expired => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidState,
                    "computer use approval receipt has expired",
                ));
            }
            ApprovalStatus::Pending => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidState,
                    "computer use approval has not been granted by a human",
                ));
            }
            ApprovalStatus::Denied | ApprovalStatus::Revoked => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidState,
                    "computer use approval is not usable",
                ));
            }
        }
        if self.binding.capability_revision != live.capability_revision {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer use approval was issued against a stale capability revision",
            ));
        }
        if self.binding.scope.run_version != live_run_version {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "computer run moved since the human approved this receipt",
            ));
        }
        if !self.binding.bounds.covers(requested) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "requested computer control exceeds the approved action classes or bounds",
            ));
        }
        Ok(())
    }

    /// Mark the receipt spent. Callers must persist before acting.
    pub fn mark_consumed(&mut self, request_id: &str, now: DateTime<Utc>) {
        self.state = ApprovalState::Consumed;
        self.consumed_at = Some(now);
        self.consumed_by_request_id = Some(request_id.to_owned());
    }

    /// Share-safe projection. Secrets and transport capabilities are absent
    /// from the type itself rather than filtered at a serialization boundary.
    pub fn project_at(&self, now: DateTime<Utc>) -> ApprovalProjection {
        ApprovalProjection {
            contract: self.contract.clone(),
            approval_id: self.approval_id.clone(),
            capability_id: self.binding.capability_id.clone(),
            capability_revision: self.binding.capability_revision.clone(),
            status: self.status_at(now),
            owner_session_id: self.binding.scope.owner_session_id,
            run_id: self.binding.scope.run_id.clone(),
            run_version: self.binding.scope.run_version,
            action_classes: self.binding.bounds.action_classes.clone(),
            max_uses: self.binding.bounds.max_uses,
            max_ttl_ms: self.binding.bounds.max_ttl_ms,
            requested_at: self.requested_at,
            request_expires_at: self.request_expires_at,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            consumed_at: self.consumed_at,
        }
    }
}

/// Redaction-safe view of an approval record.
///
/// Deliberately absent: the one-time nonce and its digest, the bearer-token
/// fingerprint, the MCP transport session id, the client actor id (which
/// embeds that session id), the workspace path, and the consuming request id.
/// A coordinator may learn that an approval exists, what bounds a human
/// granted, and where it is in its lifecycle — never anything it could
/// replay or use to locate another caller's session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalProjection {
    /// Record contract version.
    pub contract: String,
    /// Stable record identity.
    pub approval_id: String,
    /// Gated capability.
    pub capability_id: String,
    /// Capability revision bound to the record.
    pub capability_revision: String,
    /// Time-resolved status.
    pub status: ApprovalStatus,
    /// Owning session.
    pub owner_session_id: Uuid,
    /// Bound run.
    pub run_id: String,
    /// Bound run revision.
    pub run_version: u64,
    /// Approved action classes.
    pub action_classes: BTreeSet<ActionClass>,
    /// Approved action ceiling.
    pub max_uses: u32,
    /// Approved lease ceiling.
    pub max_ttl_ms: u64,
    /// When the agent asked.
    pub requested_at: DateTime<Utc>,
    /// When an unanswered request stops being answerable.
    pub request_expires_at: DateTime<Utc>,
    /// When the human decided.
    pub issued_at: Option<DateTime<Utc>>,
    /// When an issued receipt stops being consumable.
    pub expires_at: Option<DateTime<Utc>>,
    /// When the server consumed it.
    pub consumed_at: Option<DateTime<Utc>>,
}

/// Uniform "not available" failure.
///
/// Unknown approvals, wrong nonces, and cross-principal, cross-session,
/// cross-workspace, or cross-run presentations all produce this identical
/// error so the ledger cannot be probed.
pub fn not_available() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::Unauthorized,
        "computer use approval is not available to this caller",
    )
}

/// Lowercase hex SHA-256.
pub fn digest_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fresh_nonce() -> String {
    // `uuid` v4 is the crate's existing CSPRNG-backed source; two of them give
    // the full 32 bytes without adding a dependency to the trusted host.
    let mut bytes = [0u8; NONCE_BYTES];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn constant_time_hex_eq(a: &str, b: &str) -> bool {
    crate::orchestration::constant_time_eq(a.as_bytes(), b.as_bytes())
}

fn validate_fingerprint(name: &str, value: &str) -> ComputerResult<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(format!("invalid {name}")));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> ComputerError {
    ComputerError::new(ComputerErrorCode::InvalidRequest, message)
}
