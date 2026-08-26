//! Strict public projection of host authority.
//!
//! This module is a redacted, versioned DTO surface. It does not construct
//! grants, hold MAC keys, sign intent, or admit work. Host-only envelopes
//! live in the agent bridge. There is no projection-to-admission conversion.
//!
//! ```compile_fail
//! let _ = grokptah_agent_sdk::authority::MacKey::from_bytes(&[0u8; 32]);
//! ```
//!
//! ```compile_fail
//! grokptah_agent_sdk::authority::sign_intent(b"x");
//! ```
//!
//! ```compile_fail
//! fn admit(_: grokptah_agent_sdk::authority::AuthenticatedEnvelope) {}
//! ```

use serde::{Deserialize, Serialize};

/// Stable contract identifier for the public authority projection.
pub const PUBLIC_AUTHORITY_CONTRACT_VERSION: &str = "grokptah.authority.public.v1";
/// Numeric schema revision carried in every public projection.
pub const PUBLIC_AUTHORITY_SCHEMA_VERSION: u32 = 1;
/// Maximum UTF-8 bytes accepted in a public identifier.
pub const MAX_PUBLIC_IDENTITY_BYTES: usize = 128;

const REDACTION_NEEDLES: &[&str] = &[
    "BEGIN ",
    "api_key",
    "api-key",
    "authorization",
    "/Users/",
    "/home/",
    "C:\\",
    "credential_ref",
    "selection_key",
    "Bearer ",
    "mac_key",
    "hmac",
    "ldap://",
    "ldaps://",
    "uid=",
    "cn=",
    "file:",
    "clipboard",
];

/// Closed public grant class. Unknown values fail closed at parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicGrantClass {
    /// A coding or provider-backed run.
    ProviderRun,
    /// An external worker action. Not a local Run.
    ExternalWorkerAction,
    /// Read-only document inspection.
    ReadOnlyDocument,
    /// Non-persistent help answer. Must not create a session, transcript, workspace, or Run.
    HelpAnswer,
}

impl PublicGrantClass {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderRun => "provider_run",
            Self::ExternalWorkerAction => "external_worker_action",
            Self::ReadOnlyDocument => "read_only_document",
            Self::HelpAnswer => "help_answer",
        }
    }
}

/// Closed public execution lifecycle. Unknown values fail closed at parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicExecutionLifecycle {
    /// Caller intent received, not yet admitted.
    Requested,
    /// Host-minted admission exists.
    Admitted,
    /// Durable Queued Run with completed idempotency receipt.
    Queued,
    /// Attempt registered; worker has not acknowledged start.
    Starting,
    /// Worker acknowledged start.
    Running,
    /// Cooperative shutdown in progress.
    Stopping,
    /// Crash, stream loss, or uncertain send is being reconciled.
    Reconciling,
    /// Terminal success before finalization.
    Succeeded,
    /// Terminal failure before finalization.
    Failed,
    /// Terminal cancellation before finalization.
    Cancelled,
    /// Delivery or liveness cannot be proven.
    Uncertain,
    /// Terminal truth persisted; capacity may be released.
    Finalized,
}

impl PublicExecutionLifecycle {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Admitted => "admitted",
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Reconciling => "reconciling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
            Self::Finalized => "finalized",
        }
    }
}

/// Closed public send/supervision state. Unknown values fail closed at parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicSendState {
    /// Prepared and provably not sent.
    KnownNotSent,
    /// In flight; delivery is unknown.
    Sending,
    /// Delivery is unknown after a crash or cut.
    Uncertain,
    /// Provider acknowledged the request.
    Sent,
    /// Stream is being consumed.
    Streaming,
    /// Stream/result completed.
    Completed,
    /// Stream/result failed.
    Failed,
}

impl PublicSendState {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotSent => "known_not_sent",
            Self::Sending => "sending",
            Self::Uncertain => "uncertain",
            Self::Sent => "sent",
            Self::Streaming => "streaming",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// Only [`Self::KnownNotSent`] may auto-retry.
    pub const fn may_auto_retry(self) -> bool {
        matches!(self, Self::KnownNotSent)
    }
}

/// Closed identity class. Request, work, run, attempt, and related IDs stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicIdentityClass {
    /// Caller request / idempotency identity.
    Request,
    /// Admitted work identity.
    Work,
    /// Durable run identity.
    Run,
    /// Attempt identity.
    Attempt,
    /// Attempt lease identity.
    Lease,
    /// Stable provider-request identity.
    ProviderRequest,
}

/// One redacted, bounded public identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicIdentity {
    /// Closed class.
    pub class: PublicIdentityClass,
    /// Opaque identifier. Never a filesystem path.
    pub value: String,
}

impl PublicIdentity {
    /// Validate charset, UTF-8 byte ceiling, and non-path shape.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_public_id(&self.value)
    }
}

fn validate_public_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_PUBLIC_IDENTITY_BYTES {
        return Err("public identity exceeds UTF-8 byte bounds");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("public identity is not a bounded opaque identifier");
    }
    if value.contains("..") || value.contains('/') || value.contains('\\') {
        return Err("public identity must not carry a path");
    }
    Ok(())
}

/// Public revision counters. Values only; no policy text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRevisionSet {
    /// Authentication revision.
    pub auth: u64,
    /// Policy revision.
    pub policy: u64,
    /// Capability revision.
    pub capability: u64,
    /// Credential rotation counter.
    pub credential: u64,
    /// Source revision.
    pub source: u64,
    /// Provider route revision.
    pub route: u64,
}

/// One verified artifact identity. Digest is host-computed, never a trusted provider claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicArtifactIdentity {
    /// Opaque artifact identity.
    pub artifact_id: String,
    /// SHA-256 hex of streamed local bytes.
    pub digest_sha256: String,
    /// Byte length admitted after local hashing.
    pub byte_len: u64,
}

impl PublicArtifactIdentity {
    fn validate(&self) -> Result<(), &'static str> {
        validate_public_id(&self.artifact_id)?;
        if self.digest_sha256.len() != 64
            || !self
                .digest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("artifact digest is not a SHA-256 hex");
        }
        Ok(())
    }
}

/// Strict redacted projection of host authority.
///
/// Unknown fields fail closed. Secrets, local paths, provider payloads, MAC
/// tags, and unredacted policy facts are refused by [`Self::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicAuthorityProjection {
    /// Contract identifier.
    pub contract: String,
    /// Schema revision.
    pub schema_version: u32,
    /// Grant class.
    pub grant_class: PublicGrantClass,
    /// Execution lifecycle.
    pub lifecycle: PublicExecutionLifecycle,
    /// Send/supervision state.
    pub send_state: PublicSendState,
    /// Distinct identities bound to this projection.
    pub identities: Vec<PublicIdentity>,
    /// Attempt ordinal / generation.
    pub attempt_generation: u32,
    /// Lease generation / epoch.
    pub lease_generation: u64,
    /// Opaque principal, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// Opaque tenant, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Opaque project, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Opaque workspace handle. Never a path.
    pub workspace: String,
    /// Opaque session handle.
    pub session: String,
    /// Opaque agent handle, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Provider family label, never an endpoint URL.
    pub provider: String,
    /// Bounded model class, never a credential.
    pub model: String,
    /// Effort wire value, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Heartbeat freshness as unix milliseconds, when live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_unix_ms: Option<u64>,
    /// Credential/policy/capability/source/route revision tuple.
    pub revisions: PublicRevisionSet,
    /// Result source revision, when a candidate exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_revision: Option<String>,
    /// Bounded progress percent 0-100, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// Whether cancellation has been requested.
    pub cancellation_requested: bool,
    /// Whether the send/run is in operator-visible reconciliation.
    pub reconciliation_required: bool,
    /// Verified artifact identities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<PublicArtifactIdentity>,
    /// Stable public error code, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Stable public reason code, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Whether the isolated result is ready for explicit operator promotion.
    pub promotion_ready: bool,
}

impl PublicAuthorityProjection {
    /// Fail closed on unknown contract, identity bounds, or redaction needles.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != PUBLIC_AUTHORITY_CONTRACT_VERSION {
            return Err("public authority contract is not supported");
        }
        if self.schema_version != PUBLIC_AUTHORITY_SCHEMA_VERSION {
            return Err("public authority schema is not supported");
        }
        validate_public_id(&self.workspace)?;
        validate_public_id(&self.session)?;
        validate_public_id(&self.provider)?;
        validate_public_id(&self.model)?;
        for optional in [
            self.principal.as_deref(),
            self.tenant.as_deref(),
            self.project.as_deref(),
            self.agent.as_deref(),
            self.effort.as_deref(),
            self.result_revision.as_deref(),
            self.error_code.as_deref(),
            self.reason_code.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_public_id(optional)?;
        }
        for identity in &self.identities {
            identity.validate()?;
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
        }
        if self.progress.is_some_and(|value| value > 100) {
            return Err("progress exceeds 100");
        }
        if contains_redaction_needle(self) {
            return Err("public authority projection carries a redacted needle");
        }
        Ok(())
    }
}

fn contains_redaction_needle(projection: &PublicAuthorityProjection) -> bool {
    let encoded = serde_json::to_string(projection).unwrap_or_default();
    let lowered = encoded.to_ascii_lowercase();
    REDACTION_NEEDLES
        .iter()
        .any(|needle| lowered.contains(&needle.to_ascii_lowercase()) || encoded.contains(needle))
}

/// Deterministic public fixture used by host tests and brokers.
pub fn public_authority_fixture(grant_class: PublicGrantClass) -> PublicAuthorityProjection {
    let request = match grant_class {
        PublicGrantClass::HelpAnswer => "help-req-1",
        PublicGrantClass::ReadOnlyDocument => "doc-req-1",
        PublicGrantClass::ExternalWorkerAction => "ext-req-1",
        PublicGrantClass::ProviderRun => "run-req-1",
    };
    PublicAuthorityProjection {
        contract: PUBLIC_AUTHORITY_CONTRACT_VERSION.into(),
        schema_version: PUBLIC_AUTHORITY_SCHEMA_VERSION,
        grant_class,
        lifecycle: PublicExecutionLifecycle::Queued,
        send_state: PublicSendState::KnownNotSent,
        identities: vec![PublicIdentity {
            class: PublicIdentityClass::Request,
            value: request.into(),
        }],
        attempt_generation: 1,
        lease_generation: 1,
        principal: Some("usr-principal-1".into()),
        tenant: Some("tenant-1".into()),
        project: Some("project-1".into()),
        workspace: "workspace-1".into(),
        session: "session-1".into(),
        agent: None,
        provider: "xai".into(),
        model: "grok-4".into(),
        effort: None,
        heartbeat_unix_ms: None,
        revisions: PublicRevisionSet {
            auth: 1,
            policy: 1,
            capability: 1,
            credential: 1,
            source: 1,
            route: 1,
        },
        result_revision: None,
        progress: None,
        cancellation_requested: false,
        reconciliation_required: false,
        artifacts: Vec::new(),
        error_code: None,
        reason_code: None,
        promotion_ready: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_fixture_validates_and_is_secret_free() {
        for class in [
            PublicGrantClass::ProviderRun,
            PublicGrantClass::ExternalWorkerAction,
            PublicGrantClass::ReadOnlyDocument,
            PublicGrantClass::HelpAnswer,
        ] {
            let fixture = public_authority_fixture(class);
            fixture.validate().expect("fixture must validate");
            let encoded = serde_json::to_string(&fixture).unwrap();
            assert!(!contains_redaction_needle(&fixture), "{encoded}");
            assert!(!encoded.contains("hmac"));
            assert!(!encoded.contains("mac"));
            assert!(!encoded.contains("/Users/"));
        }
    }

    #[test]
    fn unknown_fields_and_path_identities_fail_closed() {
        let mut value =
            serde_json::to_value(public_authority_fixture(PublicGrantClass::HelpAnswer)).unwrap();
        value["macKey"] = serde_json::json!("deadbeef");
        assert!(serde_json::from_value::<PublicAuthorityProjection>(value).is_err());
        let mut pathy = public_authority_fixture(PublicGrantClass::ProviderRun);
        pathy.workspace = "/Users/chriscase/secret".into();
        assert!(pathy.validate().is_err());
    }

    #[test]
    fn unknown_lifecycle_and_send_enums_fail_closed() {
        let mut value =
            serde_json::to_value(public_authority_fixture(PublicGrantClass::ProviderRun)).unwrap();
        value["lifecycle"] = serde_json::json!("mystery");
        assert!(serde_json::from_value::<PublicAuthorityProjection>(value.clone()).is_err());
        value =
            serde_json::to_value(public_authority_fixture(PublicGrantClass::ProviderRun)).unwrap();
        value["sendState"] = serde_json::json!("maybe_sent");
        assert!(serde_json::from_value::<PublicAuthorityProjection>(value).is_err());
    }

    #[test]
    fn only_known_not_sent_may_auto_retry() {
        assert!(PublicSendState::KnownNotSent.may_auto_retry());
        assert!(!PublicSendState::Sending.may_auto_retry());
        assert!(!PublicSendState::Uncertain.may_auto_retry());
        assert!(!PublicSendState::Sent.may_auto_retry());
        assert!(!PublicSendState::Streaming.may_auto_retry());
        assert!(!PublicSendState::Completed.may_auto_retry());
        assert!(!PublicSendState::Failed.may_auto_retry());
    }
}
