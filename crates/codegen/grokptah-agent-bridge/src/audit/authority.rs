//! Capability authority for the two audit operations that can destroy history
//! or expose unredacted bytes (#443).
//!
//! # What this is
//!
//! Two operations are not ordinary ledger use:
//!
//! - **privileged raw export**, which copies imported v1 bytes verbatim and so
//!   can carry workspace paths, free-text `detail`, IO strings and provider
//!   material that were never redacted to the v2 rules; and
//! - **retention of a generation no verified export ever carried**, which
//!   destroys the only copy of a range.
//!
//! Before this module, both were reachable from a plain method call — one by
//! naming a different [`super::ExportScope`], the other by setting a bare
//! `allow_unexported` bool. Neither required anything of the caller, so the
//! "authority" for the deletion was the deletion's own request.
//!
//! A [`AuthorityGrant`] replaces the bool. It is authenticated under its own
//! key domain, bound to one capability, one subject and one installation,
//! expires, and is **single use**: its id is recorded in the manifest when it
//! is spent, so a captured grant authorizes nothing a second time.
//!
//! # What this is not
//!
//! This is a *structural and evidentiary* boundary, not an authenticated
//! principal boundary. There is no principal authority in this codebase yet
//! (#460/#461), so the only source a shipped build can assert today is
//! [`AuthoritySource::LocalOperator`]: an operator act on the host, not a
//! verified identity. Every grant records which source stood behind it, and
//! every tombstone written under one keeps that source permanently, so nobody
//! can later read an operator-asserted deletion as a principal-authorized one.
//! When #460/#461 land they supply an [`AuditAuthorityProvider`] that returns
//! [`AuthoritySource::Principal`]; nothing else here changes.
//!
//! The default provider is [`DeniedAuthority`], which grants nothing. A host
//! that installs no provider cannot take a privileged raw export and cannot
//! delete an unexported generation at all.

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::canon::canonical_bytes_without_mac;
use super::documents::GRANT_SCHEMA;
use super::keys::AuditKeys;
use super::{AuditError, AuditResult, RefuseReason};

/// Longest life a grant may have. A capability that outlives the operator's
/// attention is a capability nobody is watching.
pub const MAX_GRANT_TTL_SECONDS: i64 = 300;

/// Subject a privileged raw export grant is bound to.
///
/// A raw export is about the ledger as a whole rather than one generation, so
/// the subject is fixed. Single use and the TTL are what keep one grant from
/// becoming a standing licence.
pub const PRIVILEGED_RAW_EXPORT_SUBJECT: &str = "privileged-raw-export";

/// What a grant permits. Exactly one capability per grant: a grant never
/// widens, and one taken for an export can never be spent on a deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditCapability {
    /// Emit an export that carries unauthenticated legacy bytes verbatim.
    PrivilegedRawExport,
    /// Tombstone a generation that no verified export ever carried.
    RetainUnexported,
}

impl AuditCapability {
    /// Stable, secret-free operator code.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PrivilegedRawExport => "privileged_raw_export",
            Self::RetainUnexported => "retain_unexported",
        }
    }
}

impl fmt::Display for AuditCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who asserted a grant. Recorded permanently and never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoritySource {
    /// A local operator act on this host. **No authenticated principal stands
    /// behind this** — see the module docs.
    LocalOperator,
    /// Issued by an authenticated principal authority (#460/#461).
    Principal,
}

impl AuthoritySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocalOperator => "local_operator",
            Self::Principal => "principal",
        }
    }
}

impl fmt::Display for AuthoritySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the ledger asks a provider to authorize.
#[derive(Debug, Clone)]
pub struct AuthorityRequest {
    pub capability: AuditCapability,
    /// Keyed digest of the subject. Carries no path, id or scope in the clear.
    pub subject: String,
    pub installation_id: String,
}

/// Decides whether a capability may be granted on this host.
///
/// Implementations must not consult the audit ledger: the authority for
/// destroying history cannot come from the history being destroyed.
pub trait AuditAuthorityProvider: Send + Sync + fmt::Debug {
    fn authorize(&self, request: &AuthorityRequest) -> Option<AuthoritySource>;
}

/// The default: nothing is ever authorized.
///
/// A host that installs no provider can neither take a privileged raw export
/// nor delete an unexported generation. That is the correct default for a
/// process that has not been told an operator is present.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeniedAuthority;

impl AuditAuthorityProvider for DeniedAuthority {
    fn authorize(&self, _request: &AuthorityRequest) -> Option<AuthoritySource> {
        None
    }
}

/// An operator act on this host, limited to an explicit capability list.
///
/// Constructing one *is* the operator decision; the host decides whether that
/// decision was actually made (see `host::audit_authority`). It asserts
/// [`AuthoritySource::LocalOperator`] and never claims more.
#[derive(Debug, Clone)]
pub struct LocalOperatorAuthority {
    capabilities: Vec<AuditCapability>,
}

impl LocalOperatorAuthority {
    /// Authorize exactly these capabilities and no others.
    pub fn new(capabilities: impl IntoIterator<Item = AuditCapability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }
}

impl AuditAuthorityProvider for LocalOperatorAuthority {
    fn authorize(&self, request: &AuthorityRequest) -> Option<AuthoritySource> {
        self.capabilities
            .contains(&request.capability)
            .then_some(AuthoritySource::LocalOperator)
    }
}

/// A single-use, subject-bound, expiring capability grant.
///
/// The fields are private and the only way to obtain a usable one is
/// `AuditLedger::issue_authority`. Deserializing arbitrary JSON into this shape
/// is deliberately harmless: verification is mandatory at every use, and the
/// MAC is taken under a key domain no other document shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityGrant {
    schema: String,
    grant_id: String,
    installation_id: String,
    key_id: String,
    capability: AuditCapability,
    source: AuthoritySource,
    /// Keyed digest of the subject this grant is for.
    subject: String,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    mac: String,
}

impl AuthorityGrant {
    pub(crate) fn issue(
        keys: &AuditKeys,
        grant_id: String,
        capability: AuditCapability,
        source: AuthoritySource,
        subject: String,
    ) -> AuditResult<Self> {
        let issued_at = Utc::now();
        let mut grant = Self {
            schema: GRANT_SCHEMA.to_string(),
            grant_id,
            installation_id: keys.installation_id().to_string(),
            key_id: keys.key_id().to_string(),
            capability,
            source,
            subject,
            issued_at,
            expires_at: issued_at + Duration::seconds(MAX_GRANT_TTL_SECONDS),
            mac: String::new(),
        };
        let payload = canonical_bytes_without_mac(&grant)?;
        grant.mac = keys.authority_mac(&payload);
        Ok(grant)
    }

    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }

    pub fn capability(&self) -> AuditCapability {
        self.capability
    }

    pub fn source(&self) -> AuthoritySource {
        self.source
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// Check everything about this grant except whether it was already spent.
    ///
    /// Replay is caught separately, against the manifest, inside the same
    /// transaction that commits the effect — checking it here would leave a
    /// window between the check and the commit.
    pub(crate) fn check(
        &self,
        keys: &AuditKeys,
        capability: AuditCapability,
        subject: &str,
    ) -> AuditResult<()> {
        if self.schema != GRANT_SCHEMA {
            return Err(AuditError::Refused(RefuseReason::AuthorityInvalid));
        }
        // Verified before anything else is trusted, and in constant time: the
        // remaining fields are attacker-supplied until this passes.
        let payload = canonical_bytes_without_mac(self)?;
        let expected = keys.authority_mac(&payload);
        if !crate::orchestration::constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return Err(AuditError::Refused(RefuseReason::AuthorityInvalid));
        }
        if self.installation_id != keys.installation_id() || self.key_id != keys.key_id() {
            return Err(AuditError::Refused(RefuseReason::AuthorityInvalid));
        }
        if self.capability != capability || self.subject != subject {
            return Err(AuditError::Refused(RefuseReason::AuthorityScopeMismatch));
        }
        let now = Utc::now();
        // A grant that claims a life longer than the cap is rejected even if it
        // verifies: the cap is part of the contract, not a minting convenience.
        if self.expires_at - self.issued_at > Duration::seconds(MAX_GRANT_TTL_SECONDS) {
            return Err(AuditError::Refused(RefuseReason::AuthorityInvalid));
        }
        if now >= self.expires_at {
            return Err(AuditError::Refused(RefuseReason::AuthorityExpired));
        }
        Ok(())
    }
}
