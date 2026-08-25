//! Host-minted Help grants.
//!
//! The previous boundary let a caller hand in its own `Principal` and its own
//! `ServedIndex`. That is self-authorization: a renderer could assert any
//! tenant, any capability set, any project membership, and any index digest,
//! and the authority would faithfully evaluate the lie. Omitting a field made
//! it worse — an absent capability list simply meant "no capabilities", but an
//! absent *restriction* (a source whose visibility the caller chose not to
//! mention) failed open.
//!
//! A grant closes that. It is minted **only** by the host, from an already
//! authenticated principal and the host's own membership policy, against the
//! index the host is actually serving, and it is bound to a revision. The
//! renderer receives an opaque handle plus a receipt; it cannot construct a
//! grant, cannot widen one, and cannot name an index.
//!
//! Three properties:
//!
//! 1. **Unforgeable.** A grant carries a keyed MAC over every authority field.
//!    A renderer that edits any field, or fabricates a grant, fails
//!    verification — it does not silently authorize a different principal.
//! 2. **Revision-bound.** The grant names the policy revision and the index it
//!    was minted against. A grant minted before a membership change or an
//!    index rebuild is rejected, not honored.
//! 3. **Expiring and single-scope.** A grant states its action and its
//!    validity window. It cannot be replayed into a different action.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contract::{Action, Capability, Visibility};
use crate::{MAX_ID_BYTES, domain_digest, id_within_bounds};

/// Wire schema id for a minted grant.
pub const HELP_GRANT_SCHEMA: &str = "grokptah.help-grant.v1";

/// Most projects a grant may carry.
pub const MAX_GRANT_PROJECTS: usize = 64;
/// Most capabilities a grant may carry.
pub const MAX_GRANT_CAPABILITIES: usize = 16;

/// The host's minting key.
///
/// Never leaves the trusted process. The renderer holds only minted grants,
/// which is what makes a grant a capability rather than a claim.
#[derive(Clone)]
pub struct GrantMintingKey {
    secret: Vec<u8>,
}

impl std::fmt::Debug for GrantMintingKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the secret, including through a derived Debug on a
        // struct that happens to hold one.
        formatter.write_str("GrantMintingKey(redacted)")
    }
}

impl GrantMintingKey {
    /// Wrap host key material.
    ///
    /// # Errors
    /// Returns an error when the material is too short to be a real key; a
    /// short key would make forgery cheap rather than merely unlikely.
    pub fn new(secret: impl Into<Vec<u8>>) -> Result<Self, &'static str> {
        let secret = secret.into();
        if secret.len() < 32 {
            return Err("grant minting key must be at least 32 bytes");
        }
        Ok(Self { secret })
    }
}

/// What the host authenticated and resolved, before any grant exists.
///
/// This is the host's view, not the caller's. `project_ids` come from the
/// host's membership policy, never from a request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub principal_id: String,
    pub tenant_id: String,
    pub project_ids: Vec<String>,
    pub capabilities: Vec<Capability>,
    /// Revision of the membership/capability policy these were resolved from.
    pub policy_revision: String,
}

/// The index the host is actually serving, at mint time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedManifest {
    pub corpus_digest: String,
    pub index_digest: String,
    pub manifest_digest: String,
}

/// A minted grant.
///
/// Serializable so it can cross the IPC or HTTP boundary, but the renderer
/// treats it as opaque: every field is re-verified against the MAC before it
/// is believed, so editing one is equivalent to forging the whole grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelpGrant {
    pub schema: String,
    pub grant_id: String,
    pub action: Action,
    pub principal_id: String,
    pub tenant_id: String,
    pub project_ids: Vec<String>,
    pub capabilities: Vec<Capability>,
    /// Highest visibility this grant may reach. Default-deny by construction.
    pub max_visibility: Visibility,
    pub policy_revision: String,
    pub corpus_digest: String,
    pub index_digest: String,
    pub manifest_digest: String,
    /// Monotonic mint counter, compared against the host's current revision.
    pub grant_revision: u64,
    /// Validity window, in host milliseconds.
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    /// Keyed MAC over every field above.
    pub mac: String,
}

/// Why a grant was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantRejection {
    UnknownSchema,
    /// The MAC does not cover these fields: forged or edited.
    Forged,
    /// Minted for a different action than the one being attempted.
    ActionMismatch,
    /// Minted before the host's current policy or index revision.
    StaleRevision,
    /// Outside its validity window.
    Expired,
    /// Minted against a different corpus, index, or manifest.
    IndexMismatch,
    /// A field exceeded its bound.
    Bounds,
}

fn mac_fields(grant: &HelpGrant) -> Vec<String> {
    let mut fields = vec![
        grant.schema.clone(),
        grant.grant_id.clone(),
        format!("{:?}", grant.action),
        grant.principal_id.clone(),
        grant.tenant_id.clone(),
        format!("{:?}", grant.max_visibility),
        grant.policy_revision.clone(),
        grant.corpus_digest.clone(),
        grant.index_digest.clone(),
        grant.manifest_digest.clone(),
        grant.grant_revision.to_string(),
        grant.issued_at_ms.to_string(),
        grant.expires_at_ms.to_string(),
    ];
    // Sorted so an equal set always produces an equal MAC, and length-counted
    // so a project list cannot be confused with a capability list.
    let mut projects = grant.project_ids.clone();
    projects.sort();
    fields.push(projects.len().to_string());
    fields.extend(projects);
    let mut capabilities: Vec<String> = grant
        .capabilities
        .iter()
        .map(|capability| format!("{capability:?}"))
        .collect();
    capabilities.sort();
    fields.push(capabilities.len().to_string());
    fields.extend(capabilities);
    fields
}

/// Keyed MAC over a grant's authority fields.
///
/// HMAC-SHA256 built explicitly rather than pulled in as a dependency: the
/// crate already depends on `sha2` and nothing else, and a Help grant does not
/// justify widening the trusted dependency surface.
fn grant_mac(key: &GrantMintingKey, grant: &HelpGrant) -> String {
    const BLOCK: usize = 64;
    let mut normalized = [0u8; BLOCK];
    if key.secret.len() > BLOCK {
        let digest = Sha256::digest(&key.secret);
        normalized[..digest.len()].copy_from_slice(&digest);
    } else {
        normalized[..key.secret.len()].copy_from_slice(&key.secret);
    }
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }

    let fields = mac_fields(grant);
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    let payload = domain_digest("grokptah.help.grant.v1", &refs);

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(payload.as_bytes());
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    format!("hmac-sha256:{:x}", outer.finalize())
}

/// Constant-time comparison, so a rejected MAC leaks no prefix information.
fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.bytes().zip(right.bytes()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Mint a grant. Host-only.
///
/// # Errors
/// Returns an error when a field exceeds its bound, so an oversized identity
/// cannot be smuggled into a signed artifact.
pub fn mint_grant(
    key: &GrantMintingKey,
    principal: &AuthenticatedPrincipal,
    manifest: &ServedManifest,
    action: Action,
    max_visibility: Visibility,
    grant_revision: u64,
    issued_at_ms: u64,
    ttl_ms: u64,
) -> Result<HelpGrant, &'static str> {
    if !id_within_bounds(&principal.principal_id)
        || !id_within_bounds(&principal.tenant_id)
        || !id_within_bounds(&principal.policy_revision)
    {
        return Err("grant identity exceeds its bound");
    }
    if principal.project_ids.len() > MAX_GRANT_PROJECTS
        || principal.capabilities.len() > MAX_GRANT_CAPABILITIES
        || principal.project_ids.iter().any(|id| !id_within_bounds(id))
    {
        return Err("grant scope exceeds its bound");
    }
    if ttl_ms == 0 {
        return Err("grant ttl must be positive");
    }

    let grant_id = domain_digest(
        "grokptah.help.grant-id.v1",
        &[
            &principal.principal_id,
            &principal.tenant_id,
            &manifest.index_digest,
            &grant_revision.to_string(),
            &issued_at_ms.to_string(),
        ],
    );

    let mut grant = HelpGrant {
        schema: HELP_GRANT_SCHEMA.to_string(),
        grant_id,
        action,
        principal_id: principal.principal_id.clone(),
        tenant_id: principal.tenant_id.clone(),
        project_ids: principal.project_ids.clone(),
        capabilities: principal.capabilities.clone(),
        max_visibility,
        policy_revision: principal.policy_revision.clone(),
        corpus_digest: manifest.corpus_digest.clone(),
        index_digest: manifest.index_digest.clone(),
        manifest_digest: manifest.manifest_digest.clone(),
        grant_revision,
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(ttl_ms),
        mac: String::new(),
    };
    grant.mac = grant_mac(key, &grant);
    Ok(grant)
}

/// What the host currently is, when a grant is presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantAcceptance {
    pub action: Action,
    pub manifest: ServedManifest,
    pub policy_revision: String,
    pub current_revision: u64,
    pub now_ms: u64,
}

/// Verify a presented grant against what the host is right now.
///
/// # Errors
/// Returns the specific rejection. Every check fails closed: there is no path
/// that treats an unverifiable grant as a weaker but usable one.
pub fn verify_grant(
    key: &GrantMintingKey,
    grant: &HelpGrant,
    acceptance: &GrantAcceptance,
) -> Result<(), GrantRejection> {
    if grant.schema != HELP_GRANT_SCHEMA {
        return Err(GrantRejection::UnknownSchema);
    }
    if !id_within_bounds(&grant.principal_id)
        || !id_within_bounds(&grant.tenant_id)
        || grant.project_ids.len() > MAX_GRANT_PROJECTS
        || grant.capabilities.len() > MAX_GRANT_CAPABILITIES
        || grant.principal_id.len() > MAX_ID_BYTES
    {
        return Err(GrantRejection::Bounds);
    }

    // MAC first: nothing else in the grant may be believed until the whole
    // record is known to be the one the host minted.
    let expected = grant_mac(key, grant);
    if !constant_time_eq(&expected, &grant.mac) {
        return Err(GrantRejection::Forged);
    }

    if grant.action != acceptance.action {
        return Err(GrantRejection::ActionMismatch);
    }
    if grant.policy_revision != acceptance.policy_revision
        || grant.grant_revision != acceptance.current_revision
    {
        return Err(GrantRejection::StaleRevision);
    }
    if grant.corpus_digest != acceptance.manifest.corpus_digest
        || grant.index_digest != acceptance.manifest.index_digest
        || grant.manifest_digest != acceptance.manifest.manifest_digest
    {
        return Err(GrantRejection::IndexMismatch);
    }
    if acceptance.now_ms < grant.issued_at_ms || acceptance.now_ms >= grant.expires_at_ms {
        return Err(GrantRejection::Expired);
    }
    Ok(())
}
