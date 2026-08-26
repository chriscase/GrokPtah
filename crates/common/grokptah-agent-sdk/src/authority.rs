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
//! fn admit(_: grokptah_agent_sdk::authority::VerifiedEnvelope) {}
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
    "refresh_token",
    "GROKPTAH_AUTHORITY_KEY",
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
    /// Non-persistent help answer.
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

/// Closed public send/supervision state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicSendState {
    /// Prepared and provably not sent.
    KnownNotSent,
    /// In flight; delivery is unknown.
    Sending,
    /// Provider acknowledged the request.
    Sent,
    /// Delivery is unknown after a crash or cut.
    Uncertain,
    /// Acknowledged and a response is being consumed.
    Responding,
    /// Terminal for this identity.
    Settled,
}

impl PublicSendState {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotSent => "known_not_sent",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Uncertain => "uncertain",
            Self::Responding => "responding",
            Self::Settled => "settled",
        }
    }

    /// Only [`Self::KnownNotSent`] may auto-retry.
    pub const fn may_auto_retry(self) -> bool {
        matches!(self, Self::KnownNotSent)
    }
}

/// Closed identity class. These classes must never be substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Worker liveness identity.
    WorkerLiveness,
    /// Finalization identity.
    Finalization,
    /// Receipt identity.
    Receipt,
    /// Tombstone identity.
    Tombstone,
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
        if self.value.is_empty() || self.value.len() > MAX_PUBLIC_IDENTITY_BYTES {
            return Err("public identity exceeds its bound");
        }
        if !self
            .value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err("public identity is not opaque");
        }
        if self.value.contains("..")
            || self.value.contains('/')
            || self.value.contains('\\')
            || REDACTION_NEEDLES
                .iter()
                .any(|needle| self.value.contains(needle))
        {
            return Err("public identity is not share-safe");
        }
        Ok(())
    }
}

/// Redacted revision counters. No signing material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRevisionSet {
    /// Authentication revision.
    pub auth: u64,
    /// Policy revision.
    pub policy: u64,
    /// Capability revision.
    pub capability: u64,
    /// Credential revision.
    pub credential: u64,
    /// Source revision.
    pub source: u64,
}

/// Share-safe public authority projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicAuthorityProjection {
    /// Contract identifier.
    pub contract: String,
    /// Schema revision.
    pub schema_version: u32,
    /// Closed grant class.
    pub grant_class: PublicGrantClass,
    /// Closed send state.
    pub send_state: PublicSendState,
    /// Distinct identities.
    pub identities: Vec<PublicIdentity>,
    /// Acting principal, when share-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// Tenant, when share-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Project, when share-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Workspace handle, never a path.
    pub workspace: String,
    /// Session handle.
    pub session: String,
    /// Agent handle, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Provider family.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Effort, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Revision counters.
    pub revisions: PublicRevisionSet,
}

impl PublicAuthorityProjection {
    /// Fail closed if any field is a secret, path, or MAC.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != PUBLIC_AUTHORITY_CONTRACT_VERSION {
            return Err("unknown public authority contract");
        }
        if self.schema_version != PUBLIC_AUTHORITY_SCHEMA_VERSION {
            return Err("unknown public authority schema");
        }
        for identity in &self.identities {
            identity.validate()?;
        }
        for field in [
            Some(self.workspace.as_str()),
            Some(self.session.as_str()),
            Some(self.provider.as_str()),
            Some(self.model.as_str()),
            self.principal.as_deref(),
            self.tenant.as_deref(),
            self.project.as_deref(),
            self.agent.as_deref(),
            self.effort.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if field.is_empty() || field.len() > MAX_PUBLIC_IDENTITY_BYTES {
                return Err("public field exceeds its bound");
            }
            if REDACTION_NEEDLES
                .iter()
                .any(|needle| field.contains(needle))
            {
                return Err("public field is not share-safe");
            }
        }
        let encoded = serde_json::to_string(self).map_err(|_| "public projection is not json")?;
        if REDACTION_NEEDLES
            .iter()
            .any(|needle| encoded.contains(needle))
        {
            return Err("public projection leaked a secret needle");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_projection_rejects_path_and_mac_needles() {
        let mut projection = PublicAuthorityProjection {
            contract: PUBLIC_AUTHORITY_CONTRACT_VERSION.into(),
            schema_version: PUBLIC_AUTHORITY_SCHEMA_VERSION,
            grant_class: PublicGrantClass::ProviderRun,
            send_state: PublicSendState::KnownNotSent,
            identities: vec![PublicIdentity {
                class: PublicIdentityClass::Run,
                value: "run-1".into(),
            }],
            principal: Some("prn-1".into()),
            tenant: Some("tnt-1".into()),
            project: Some("prj-1".into()),
            workspace: "wsp-1".into(),
            session: "ses-1".into(),
            agent: None,
            provider: "xai".into(),
            model: "grok-4".into(),
            effort: None,
            revisions: PublicRevisionSet {
                auth: 1,
                policy: 1,
                capability: 1,
                credential: 1,
                source: 1,
            },
        };
        projection.validate().expect("share-safe projection");
        projection.workspace = "/Users/secret".into();
        assert!(projection.validate().is_err());
    }

    #[test]
    fn only_known_not_sent_may_auto_retry() {
        for state in [
            PublicSendState::KnownNotSent,
            PublicSendState::Sending,
            PublicSendState::Sent,
            PublicSendState::Uncertain,
            PublicSendState::Responding,
            PublicSendState::Settled,
        ] {
            assert_eq!(
                state.may_auto_retry(),
                state == PublicSendState::KnownNotSent
            );
        }
    }
}
