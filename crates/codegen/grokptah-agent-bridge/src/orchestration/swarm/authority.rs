//! Host-minted, action-time authority for one exact dispatch attempt.
//!
//! Nothing here mints a credential. An `ActionAuthority` is a durable capability
//! *statement*: it names the exact workspace, session, agent, provider route,
//! capability and policy revisions, execution bounds, and attempt identity that
//! a single action is permitted to exercise. It is written before it is
//! returned, so authority can never exist only in memory, and it is consumed
//! exactly once through a durable one-winner claim.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::completion::CompletionUsage;
use crate::gateway_config::{
    ComputerUseTier, ModelCapabilities, ProviderDeadlineClass, ProviderDialect, ProviderKind,
};
use crate::orchestration::types::{hash_payload, OrchError, OrchErrorCode, RunBounds};
use crate::types::EffortLevel;

use super::ids::{AttemptId, AuthorityId, GraphId, WorkId, WorkerId};
use super::spec::WorkCapability;

pub const AUTHORITY_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_ROUTE_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_ATTEMPT_SCHEMA_VERSION: u32 = 1;

const MAX_ROUTE_VALUE_BYTES: usize = 2_048;

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn conflict(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Conflict, message)
}

fn unauthorized(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::ForbiddenScope, message)
}

fn check_route_value(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > MAX_ROUTE_VALUE_BYTES || value.contains('\0') {
        return Err(invalid(format!("provider route {field} is invalid")));
    }
    Ok(())
}

/// Immutable, non-secret provider route frozen before an attempt exists.
///
/// Dispatch must read this record rather than consulting mutable environment,
/// catalog, or provider-profile state, so a profile edited mid-flight cannot
/// retroactively change what an in-flight attempt was authorized to reach.
/// `credential_ref` is an opaque keychain reference and `credential_fingerprint`
/// binds the route to a credential identity without persisting bearer material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRouteSnapshot {
    pub schema_version: u32,
    pub provider_id: String,
    pub profile_id: String,
    pub model_id: String,
    pub wire_model_id: String,
    pub kind: ProviderKind,
    pub dialect: ProviderDialect,
    pub base_url: String,
    pub endpoint_fingerprint: String,
    pub credential_ref: String,
    pub credential_fingerprint: String,
    pub capabilities: ModelCapabilities,
    pub deadline_class: ProviderDeadlineClass,
    pub effort: EffortLevel,
    pub snapshot_hash: String,
}

impl ProviderRouteSnapshot {
    fn expected_endpoint_fingerprint(&self) -> String {
        hash_payload(&serde_json::json!({
            "kind": self.kind,
            "dialect": self.dialect,
            "baseUrl": self.base_url,
        }))
    }

    fn hash_material(&self) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": self.schema_version,
            "providerId": self.provider_id,
            "profileId": self.profile_id,
            "modelId": self.model_id,
            "wireModelId": self.wire_model_id,
            "kind": self.kind,
            "dialect": self.dialect,
            "baseUrl": self.base_url,
            "endpointFingerprint": self.endpoint_fingerprint,
            "credentialRef": self.credential_ref,
            "credentialFingerprint": self.credential_fingerprint,
            "capabilities": self.capabilities,
            "deadlineClass": self.deadline_class,
            "effort": self.effort,
        })
    }

    /// Recompute the derived fields and seal the snapshot.
    pub fn seal(mut self) -> Result<Self, OrchError> {
        self.endpoint_fingerprint = self.expected_endpoint_fingerprint();
        self.snapshot_hash = hash_payload(&self.hash_material());
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != PROVIDER_ROUTE_SCHEMA_VERSION {
            return Err(invalid("provider route schema version is not supported"));
        }
        for (value, field) in [
            (self.provider_id.as_str(), "provider_id"),
            (self.profile_id.as_str(), "profile_id"),
            (self.model_id.as_str(), "model_id"),
            (self.wire_model_id.as_str(), "wire_model_id"),
            (self.base_url.as_str(), "base_url"),
            (self.endpoint_fingerprint.as_str(), "endpoint_fingerprint"),
            (self.credential_ref.as_str(), "credential_ref"),
            (
                self.credential_fingerprint.as_str(),
                "credential_fingerprint",
            ),
            (self.snapshot_hash.as_str(), "snapshot_hash"),
        ] {
            check_route_value(value, field)?;
        }
        if self.endpoint_fingerprint != self.expected_endpoint_fingerprint() {
            return Err(invalid(
                "provider route endpoint fingerprint is inconsistent",
            ));
        }
        if self.snapshot_hash != hash_payload(&self.hash_material()) {
            return Err(invalid("provider route snapshot hash is inconsistent"));
        }
        Ok(())
    }

    /// Stable, secret-free attribution key for mixed-provider accounting.
    pub fn attribution_key(&self) -> String {
        format!(
            "{}/{}/{}@{}",
            self.provider_id,
            self.profile_id,
            self.model_id,
            self.effort.as_str()
        )
    }
}

/// The exact revisions a decision was made under.
///
/// A capability or policy edit bumps its revision. An authority minted under an
/// older revision is refused at use, so a widened policy cannot retroactively
/// bless an action and a narrowed one cannot be outrun by an in-flight attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRevisions {
    pub capability_revision: u64,
    pub policy_revision: u64,
}

/// Everything one action is permitted to do, bound to exactly one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionAuthority {
    pub schema_version: u32,
    pub authority_id: AuthorityId,
    pub graph_id: GraphId,
    pub work_id: WorkId,
    pub worker_id: WorkerId,
    pub attempt_id: AttemptId,
    /// Monotonic attempt ordinal within the work item. Part of the identity so
    /// a replayed attempt cannot present a prior attempt's authority.
    pub attempt: u32,
    pub session_id: Uuid,
    /// Canonical workspace path this authority is valid in.
    pub workspace: String,
    pub agent_id: String,
    pub route: ProviderRouteSnapshot,
    pub revisions: PolicyRevisions,
    pub capabilities: BTreeSet<WorkCapability>,
    pub bounds: RunBounds,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Binding digest over every field above. Recomputed at use.
    pub binding_hash: String,
}

/// What the caller claims at the moment it wants to act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityUse<'a> {
    pub graph_id: &'a GraphId,
    pub work_id: &'a WorkId,
    pub attempt_id: &'a AttemptId,
    pub attempt: u32,
    pub session_id: Uuid,
    pub workspace: &'a str,
    pub agent_id: &'a str,
    pub route_snapshot_hash: &'a str,
    pub revisions: PolicyRevisions,
    pub capability: WorkCapability,
}

impl ActionAuthority {
    fn hash_material(&self) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": self.schema_version,
            "authorityId": self.authority_id,
            "graphId": self.graph_id,
            "workId": self.work_id,
            "workerId": self.worker_id,
            "attemptId": self.attempt_id,
            "attempt": self.attempt,
            "sessionId": self.session_id,
            "workspace": self.workspace,
            "agentId": self.agent_id,
            "routeSnapshotHash": self.route.snapshot_hash,
            "revisions": self.revisions,
            "capabilities": self.capabilities,
            "bounds": self.bounds,
            "issuedAt": self.issued_at,
            "expiresAt": self.expires_at,
        })
    }

    /// Seal the authority by computing its binding digest.
    pub fn seal(mut self) -> Result<Self, OrchError> {
        self.route = self.route.clone().seal()?;
        self.binding_hash = hash_payload(&self.hash_material());
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != AUTHORITY_SCHEMA_VERSION {
            return Err(invalid("authority schema version is not supported"));
        }
        self.authority_id.validate()?;
        self.graph_id.validate()?;
        self.work_id.validate()?;
        self.worker_id.validate()?;
        self.attempt_id.validate()?;
        self.route.validate()?;
        self.bounds.validate()?;
        if self.workspace.is_empty() || self.workspace.len() > 4096 {
            return Err(invalid("authority workspace is invalid"));
        }
        if self.agent_id.is_empty() || self.agent_id.len() > 256 {
            return Err(invalid("authority agent id is invalid"));
        }
        if self.capabilities.is_empty() {
            return Err(invalid("authority grants no capability"));
        }
        if self.expires_at <= self.issued_at {
            return Err(invalid("authority has a non-positive lifetime"));
        }
        if self.binding_hash != hash_payload(&self.hash_material()) {
            return Err(invalid("authority binding hash is inconsistent"));
        }
        Ok(())
    }

    /// Verify this authority against what the caller claims, at `now`.
    ///
    /// Every mismatch is a refusal. There is no partial credit: an authority
    /// that names another workspace, session, agent, route, revision pair, or
    /// attempt is simply not this action's authority.
    pub fn verify(&self, claim: &AuthorityUse<'_>, now: DateTime<Utc>) -> Result<(), OrchError> {
        self.validate()?;
        if now < self.issued_at {
            return Err(unauthorized("authority is not yet valid"));
        }
        if now >= self.expires_at {
            return Err(unauthorized("authority has expired"));
        }
        if &self.graph_id != claim.graph_id
            || &self.work_id != claim.work_id
            || &self.attempt_id != claim.attempt_id
            || self.attempt != claim.attempt
        {
            return Err(unauthorized("authority is bound to a different attempt"));
        }
        if self.session_id != claim.session_id {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "authority is bound to a different session",
            ));
        }
        if self.workspace != claim.workspace {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "authority is bound to a different workspace",
            ));
        }
        if self.agent_id != claim.agent_id {
            return Err(unauthorized("authority is bound to a different agent"));
        }
        if self.route.snapshot_hash != claim.route_snapshot_hash {
            return Err(unauthorized(
                "authority is bound to a different provider route",
            ));
        }
        if self.revisions != claim.revisions {
            return Err(OrchError::new(
                OrchErrorCode::StaleVersion,
                "authority was minted under different capability or policy revisions",
            ));
        }
        if !self.capabilities.contains(&claim.capability) {
            return Err(unauthorized("capability is outside this authority"));
        }
        Ok(())
    }
}

/// Namespace for content-derived identities. Fixed forever: changing it would
/// make a replayed attempt mint a second identity.
pub const IDENTITY_NAMESPACE: &str = "grokptah.swarm.v1";

/// Deterministic attempt identity.
///
/// Identity is a pure function of graph, work, and attempt ordinal, so
/// replaying a planning pass after a restart proposes the identifier already on
/// disk instead of minting a second one.
pub fn derive_attempt_id(
    graph_id: &GraphId,
    work_id: &WorkId,
    attempt: u32,
) -> Result<AttemptId, OrchError> {
    let digest = hash_payload(&serde_json::json!({
        "namespace": IDENTITY_NAMESPACE,
        "kind": "attempt",
        "graphId": graph_id,
        "workId": work_id,
        "attempt": attempt,
    }));
    AttemptId::parse(format!("att-{digest}"))
}

/// Deterministic authority identity for one attempt and revision pair.
///
/// Re-minting under the same revisions returns the same identifier, so a replay
/// reuses the durable record instead of creating a second live authority.
pub fn derive_authority_id(
    attempt_id: &AttemptId,
    revisions: PolicyRevisions,
) -> Result<AuthorityId, OrchError> {
    let digest = hash_payload(&serde_json::json!({
        "namespace": IDENTITY_NAMESPACE,
        "kind": "authority",
        "attemptId": attempt_id,
        "capabilityRevision": revisions.capability_revision,
        "policyRevision": revisions.policy_revision,
    }));
    AuthorityId::parse(format!("aut-{digest}"))
}

/// Whether request bytes reached the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendCertainty {
    /// Proven that no request bytes left the process.
    KnownNotSent,
    /// Proven accepted, with a response.
    KnownAccepted,
    /// The send may or may not have been accepted.
    UncertainAccept,
}

/// What a caller is permitted to do after an attempt finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    /// Safe to retry inside the same work item.
    SameWorkSafe,
    /// Only an explicit, operator-visible new attempt is permitted.
    ExplicitNewAttemptOnly,
}

impl SendCertainty {
    pub fn retry_class(self) -> RetryClass {
        match self {
            Self::KnownNotSent => RetryClass::SameWorkSafe,
            Self::KnownAccepted | Self::UncertainAccept => RetryClass::ExplicitNewAttemptOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// The row exists and the host may enter the transport.
    Admitted,
    /// The transport returned and the outcome is recorded.
    Finished,
}

/// Durable record of one provider dispatch attempt.
///
/// The row is installed before the host enters the transport. If the process
/// disappears while it is `Admitted`, restart conservatively treats the send as
/// possibly accepted, because only an outcome proven not to have put bytes on
/// the wire is safe to repeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptRecord {
    pub schema_version: u32,
    pub attempt_id: AttemptId,
    pub graph_id: GraphId,
    pub work_id: WorkId,
    pub attempt: u32,
    pub authority_id: AuthorityId,
    pub route_snapshot_hash: String,
    /// Secret-free provider attribution key for mixed-provider accounting.
    pub attribution_key: String,
    /// Per-work send ordinal, claimed with `checked_add`. A work item that
    /// exhausts the ordinal space stops rather than reusing a position.
    pub ordinal: u64,
    pub state: AttemptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_certainty: Option<SendCertainty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_class: Option<RetryClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompletionUsage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

impl ProviderAttemptRecord {
    pub fn admitted(
        authority: &ActionAuthority,
        ordinal: u64,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        authority.validate()?;
        let record = Self {
            schema_version: PROVIDER_ATTEMPT_SCHEMA_VERSION,
            attempt_id: authority.attempt_id.clone(),
            graph_id: authority.graph_id.clone(),
            work_id: authority.work_id.clone(),
            attempt: authority.attempt,
            authority_id: authority.authority_id.clone(),
            route_snapshot_hash: authority.route.snapshot_hash.clone(),
            attribution_key: authority.route.attribution_key(),
            ordinal,
            state: AttemptState::Admitted,
            send_certainty: None,
            retry_class: None,
            http_status: None,
            usage: None,
            created_at: now,
            updated_at: now,
            finished_at: None,
        };
        record.validate()?;
        Ok(record)
    }

    /// Record the transport outcome. Repeating the identical outcome is a
    /// no-op so a replayed completion is safe; a different one is a conflict.
    pub fn finish(
        &mut self,
        certainty: SendCertainty,
        http_status: Option<u16>,
        usage: Option<CompletionUsage>,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        if self.state == AttemptState::Finished {
            if self.send_certainty == Some(certainty)
                && self.http_status == http_status
                && self.usage == usage
            {
                return Ok(());
            }
            return Err(conflict("attempt completion conflicts with durable state"));
        }
        if certainty != SendCertainty::KnownAccepted && usage.is_some() {
            return Err(invalid("provider usage requires a known accepted response"));
        }
        if certainty == SendCertainty::KnownNotSent && http_status.is_some() {
            return Err(invalid(
                "a known-not-sent attempt cannot carry an HTTP status",
            ));
        }
        self.state = AttemptState::Finished;
        self.send_certainty = Some(certainty);
        self.retry_class = Some(certainty.retry_class());
        self.http_status = http_status;
        self.usage = usage;
        self.updated_at = self.updated_at.max(now);
        self.finished_at = Some(self.updated_at);
        self.validate()
    }

    /// Restart recovery: an attempt still `Admitted` may have been accepted.
    pub fn recover_uncertain(&mut self, now: DateTime<Utc>) -> Result<bool, OrchError> {
        if self.state == AttemptState::Finished {
            return Ok(false);
        }
        self.finish(SendCertainty::UncertainAccept, None, None, now)?;
        Ok(true)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != PROVIDER_ATTEMPT_SCHEMA_VERSION {
            return Err(invalid("provider attempt schema version is not supported"));
        }
        self.attempt_id.validate()?;
        self.graph_id.validate()?;
        self.work_id.validate()?;
        self.authority_id.validate()?;
        check_route_value(&self.route_snapshot_hash, "route_snapshot_hash")?;
        check_route_value(&self.attribution_key, "attribution_key")?;
        if self.state == AttemptState::Finished && self.send_certainty.is_none() {
            return Err(invalid("a finished attempt must record its send certainty"));
        }
        if self.send_certainty.map(SendCertainty::retry_class) != self.retry_class {
            return Err(invalid("attempt retry class disagrees with its certainty"));
        }
        Ok(())
    }

    /// True when this attempt must not be repeated inside the same work item.
    pub fn forbids_same_work_retry(&self) -> bool {
        !matches!(self.retry_class, Some(RetryClass::SameWorkSafe))
            && self.state == AttemptState::Finished
    }
}

/// Highest Computer Use authority this route is measured to hold.
pub fn route_computer_use_tier(route: &ProviderRouteSnapshot) -> ComputerUseTier {
    route.capabilities.computer_use_tier
}
