//! Durable record shapes for the one canonical authority store.
//!
//! Deliberate omissions, each of which was a live defect in a donor branch:
//!
//! * No `#[serde(default)]` on any authority-bearing field. A record written
//!   by an older build that lacks a field does not silently acquire a
//!   permissive value — it fails to parse and the store refuses service with
//!   [`crate::AuthorityError::CorruptState`].
//! * No `#[serde(alias)]` for renamed fields, so a record cannot be reinterpreted
//!   under a newer field's meaning.
//! * No optional credential fingerprint. A credential record always pins the
//!   secret it authenticates; there is no "fingerprint absent, accept anything"
//!   path.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA_VERSION: u32 = 1;

/// One credential belonging to one principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredCredential {
    /// Caller-visible name of the credential slot (e.g. "laptop").
    pub(crate) credential_id: String,
    /// Host-issued principal identity, stable across rotation.
    pub(crate) principal: String,
    /// Incarnation of the current secret. Rotates on every secret change.
    pub(crate) incarnation: String,
    /// Authentication generation. Advances on every secret change.
    pub(crate) auth_generation: u64,
    /// Digest of the secret this record authenticates. Always present.
    pub(crate) credential_fingerprint: String,
    /// Owner account this principal belongs to.
    pub(crate) owner_id: String,
}

/// A host-issued resource and the authority that owns it.
///
/// Presence in this map is proof the *host* created the resource. There is no
/// operation that inserts a resource on a caller's behalf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredResource {
    pub(crate) incarnation: String,
    pub(crate) principal: String,
    pub(crate) credential_incarnation: String,
    pub(crate) auth_generation: u64,
    pub(crate) session: String,
    pub(crate) workspace: String,
    pub(crate) control_epoch: u64,
    /// Latest accepted observation revision for this surface.
    pub(crate) observation_revision: u64,
    /// Digest of the latest accepted observation.
    pub(crate) observation_digest: String,
}

/// A sealed capability grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredCapability {
    pub(crate) capability_id: String,
    pub(crate) principal: String,
    pub(crate) credential_incarnation: String,
    pub(crate) auth_generation: u64,
    pub(crate) capability_generation: u64,
    pub(crate) session: String,
    pub(crate) workspace: String,
    pub(crate) resource: String,
    pub(crate) control_epoch: u64,
    /// Who stands behind this grant. Required: a record without it does not
    /// parse, rather than defaulting to operator authority.
    pub(crate) actor: String,
    /// Effect class this grant covers.
    pub(crate) effect: String,
    /// Wall-clock expiry, milliseconds since the Unix epoch.
    pub(crate) expires_at_ms: u64,
    /// Whether the grant has been spent. Sealed grants are one-use.
    pub(crate) consumed: bool,
}

/// A one-use effect lease derived from a sealed capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredLease {
    pub(crate) lease_id: String,
    pub(crate) capability_id: String,
    pub(crate) principal: String,
    pub(crate) credential_incarnation: String,
    pub(crate) auth_generation: u64,
    pub(crate) capability_generation: u64,
    pub(crate) session: String,
    pub(crate) workspace: String,
    pub(crate) resource: String,
    pub(crate) control_epoch: u64,
    pub(crate) observation_revision: u64,
    pub(crate) observation_digest: String,
    /// Digest of the exact action this lease authorises.
    pub(crate) action_digest: String,
    pub(crate) actor: String,
    pub(crate) effect: String,
    pub(crate) expires_at_ms: u64,
    pub(crate) consumed: bool,
}

/// The durable record of one provider physical-send attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredAttempt {
    pub(crate) attempt_id: String,
    pub(crate) lease_id: String,
    pub(crate) principal: String,
    pub(crate) credential_incarnation: String,
    pub(crate) auth_generation: u64,
    pub(crate) capability_generation: u64,
    pub(crate) session: String,
    pub(crate) workspace: String,
    pub(crate) resource: String,
    pub(crate) control_epoch: u64,
    /// Digest over URL, method, dialect, credential, model, and body.
    pub(crate) request_digest: String,
    /// Body digest alone, for audit correlation.
    pub(crate) body_digest: String,
    pub(crate) actor: String,
    /// Idempotency key offered to the provider, when it supports one.
    pub(crate) idempotency_key: String,
    pub(crate) state: String,
    /// Set once the attempt reaches a terminal or ambiguous state.
    pub(crate) settlement: Option<String>,
}

/// The whole durable authority root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredAuthority {
    pub(crate) schema_version: u32,
    pub(crate) owner_id: String,
    /// Advances whenever the host control plane re-arms.
    pub(crate) control_epoch: u64,
    /// Advances on capability policy rotation or revocation.
    pub(crate) capability_generation: u64,
    /// Next authentication generation to hand out.
    pub(crate) next_auth_generation: u64,
    pub(crate) credentials: Vec<StoredCredential>,
    pub(crate) resources: BTreeMap<String, StoredResource>,
    pub(crate) capabilities: BTreeMap<String, StoredCapability>,
    pub(crate) leases: BTreeMap<String, StoredLease>,
    pub(crate) attempts: BTreeMap<String, StoredAttempt>,
}

impl StoredAuthority {
    pub(crate) fn new(owner_id: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            owner_id: owner_id.to_string(),
            control_epoch: 1,
            capability_generation: 1,
            next_auth_generation: 1,
            credentials: Vec::new(),
            resources: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            leases: BTreeMap::new(),
            attempts: BTreeMap::new(),
        }
    }
}
