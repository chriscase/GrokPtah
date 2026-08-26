//! Host-only execution specification, HMAC-SHA-256 intent seal, and grants.
//!
//! Public SDK types in `grokptah_agent_sdk::authority` are a redacted
//! projection. There is no grant constructor, MAC-key API, or signer on the
//! SDK crate, and no deserialization path from a public projection into
//! admission. Privileged grants are derived only from [`VerifiedSpec`].

use std::collections::BTreeSet;
use std::fmt;

use grokptah_agent_sdk::authority::{
    PublicAuthorityProjection, PublicExecutionLifecycle, PublicGrantClass, PublicIdentity,
    PublicIdentityClass, PublicRevisionSet, PublicSendState, PUBLIC_AUTHORITY_CONTRACT_VERSION,
    PUBLIC_AUTHORITY_SCHEMA_VERSION,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Domain for the sealed execution specification.
pub const MAC_DOMAIN_SPEC: &str = "grokptah.authority.spec.v1";
/// Canonical encoding version.
pub const MAC_ENCODING_VERSION: u32 = 1;
const MAC_MAGIC: &[u8] = b"GPTA.MAC.v1";
const MIN_KEY_BYTES: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_FIELD_BYTES: usize = 4 * 1024;
const MAX_CAPABILITY_BYTES: usize = 8 * 1024;
const MAX_DIGEST_HEX: usize = 64;

type HmacSha256 = Hmac<Sha256>;

/// Closed spine error. No privileged payload is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineError {
    /// Identity missing, duplicated, substituted, or ill-formed.
    InvalidIdentity,
    /// Two identity values collided or a required class was repeated.
    DuplicateIdentity,
    /// Scope fields do not bind to the same chain.
    CrossScope,
    /// Observed revision is behind the sealed revision.
    StaleRevision,
    /// Checked revision increment overflowed.
    RevisionOverflow,
    /// Unknown JSON field, closed enum variant, or duplicate canonical key.
    UnknownField,
    /// HMAC did not verify or encoding was substituted.
    MacInvalid,
    /// Transition is not permitted from the current state.
    TransitionForbidden,
    /// Help grant attempted a durable coding artifact.
    HelpCannotCreateDurable,
    /// Required bounds were omitted.
    BoundsOmitted,
    /// UTF-8 byte ceiling exceeded or malformed encoding.
    Utf8Ceiling,
    /// Supervisor capacity is exhausted or not owned.
    Capacity,
    /// Key material is shorter than 256 bits or entropy failed.
    WeakKey,
    /// Auto-retry was requested from a state other than KnownNotSent.
    AutoRetryForbidden,
}

impl SpineError {
    /// Stable label for tests and evidence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIdentity => "invalid_identity",
            Self::DuplicateIdentity => "duplicate_identity",
            Self::CrossScope => "cross_scope",
            Self::StaleRevision => "stale_revision",
            Self::RevisionOverflow => "revision_overflow",
            Self::UnknownField => "unknown_field",
            Self::MacInvalid => "mac_invalid",
            Self::TransitionForbidden => "transition_forbidden",
            Self::HelpCannotCreateDurable => "help_cannot_create_durable",
            Self::BoundsOmitted => "bounds_omitted",
            Self::Utf8Ceiling => "utf8_ceiling",
            Self::Capacity => "capacity",
            Self::WeakKey => "weak_key",
            Self::AutoRetryForbidden => "auto_retry_forbidden",
        }
    }
}

/// Host-only HMAC-SHA-256 key. Zeroized on drop. Never projected.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MacKey {
    bytes: Vec<u8>,
}

impl fmt::Debug for MacKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacKey")
            .field("bits", &(self.bytes.len().saturating_mul(8)))
            .finish()
    }
}

impl MacKey {
    /// Accept host-only key material of at least 256 bits.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SpineError> {
        if bytes.len() < MIN_KEY_BYTES {
            return Err(SpineError::WeakKey);
        }
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    /// Generate a host-only key using OS entropy. Fails closed on entropy loss.
    pub fn generate() -> Result<Self, SpineError> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| SpineError::WeakKey)?;
        Self::from_bytes(&bytes)
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Checked revision counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    /// Construct a revision.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Observed value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Checked increment.
    pub fn checked_next(self) -> Result<Self, SpineError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(SpineError::RevisionOverflow)
    }

    /// Fail closed when `observed` is not exactly `self`.
    pub fn require_current(self, observed: Self) -> Result<(), SpineError> {
        if self == observed {
            Ok(())
        } else if observed.0 < self.0 {
            Err(SpineError::StaleRevision)
        } else {
            Err(SpineError::InvalidIdentity)
        }
    }
}

/// Required accepted bounds. Omission of any field fails serde deny_unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedBounds {
    /// UTF-8 byte ceiling for prompt/intent.
    pub max_prompt_bytes: u32,
    /// Maximum model rounds.
    pub max_rounds: u32,
    /// Wall duration in milliseconds.
    pub max_duration_ms: u64,
}

impl AcceptedBounds {
    /// Validate non-zero required ceilings.
    pub fn validate(&self) -> Result<(), SpineError> {
        if self.max_prompt_bytes == 0 || self.max_rounds == 0 || self.max_duration_ms == 0 {
            return Err(SpineError::BoundsOmitted);
        }
        Ok(())
    }
}

/// Host grant class. Help is non-persistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGrantClass {
    /// Provider-backed coding run.
    ProviderRun,
    /// External worker action.
    ExternalWorkerAction,
    /// Read-only document.
    ReadOnlyDocument,
    /// Help answer that must not create durable coding artifacts.
    HelpAnswer,
}

impl HostGrantClass {
    fn label(self) -> &'static str {
        match self {
            Self::ProviderRun => "provider_run",
            Self::ExternalWorkerAction => "external_worker_action",
            Self::ReadOnlyDocument => "read_only_document",
            Self::HelpAnswer => "help_answer",
        }
    }

    fn as_public(self) -> PublicGrantClass {
        match self {
            Self::ProviderRun => PublicGrantClass::ProviderRun,
            Self::ExternalWorkerAction => PublicGrantClass::ExternalWorkerAction,
            Self::ReadOnlyDocument => PublicGrantClass::ReadOnlyDocument,
            Self::HelpAnswer => PublicGrantClass::HelpAnswer,
        }
    }
}

/// Derived host grant. Constructible only from [`VerifiedSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostGrant {
    /// Provider run grant.
    ProviderRun {
        /// Run identity.
        run_id: String,
        /// Work identity.
        work_id: String,
    },
    /// External worker action grant.
    ExternalWorkerAction {
        /// Provider request identity.
        provider_request_id: String,
    },
    /// Read-only document grant.
    ReadOnlyDocument {
        /// Request identity.
        request_id: String,
    },
    /// Non-persistent help grant.
    HelpAnswer {
        /// Request identity.
        request_id: String,
    },
}

/// Host-owned immutable execution specification.
///
/// Every security-relevant field is first-class. There is no caller-authored
/// identity bag and no optional omission of revisions, bounds, or IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalExecutionSpec {
    /// Opaque installation-local principal.
    pub principal: String,
    /// Tenant.
    pub tenant: String,
    /// Project.
    pub project: String,
    /// Opaque workspace identity. Never a filesystem path.
    pub workspace_id: String,
    /// Immutable source revision (git SHA or equivalent opaque id).
    pub workspace_source_revision: String,
    /// Session identity.
    pub session: String,
    /// Agent identity.
    pub agent: String,
    /// Objective digest (SHA-256 hex).
    pub objective_digest: String,
    /// Bounded private input digest (SHA-256 hex). Never the prompt bytes.
    pub input_digest: String,
    /// Provider family.
    pub provider: String,
    /// Provider profile identity.
    pub provider_profile: String,
    /// Base endpoint fingerprint, never a URL.
    pub endpoint_fingerprint: String,
    /// Model identity.
    pub model: String,
    /// Effort / reasoning class.
    pub effort: String,
    /// Execution bounds.
    pub bounds: AcceptedBounds,
    /// Sorted unique capability identifiers.
    pub capability_set: Vec<String>,
    /// Policy revision.
    pub policy_revision: Revision,
    /// Capability revision.
    pub capability_revision: Revision,
    /// Auth revision.
    pub auth_revision: Revision,
    /// Credential identity. Never locator bytes.
    pub credential_id: String,
    /// Credential revision.
    pub credential_revision: Revision,
    /// Route revision.
    pub route_revision: Revision,
    /// Source / corpus revision counter. Distinct from the git SHA.
    pub source_revision: Revision,
    /// Caller request / idempotency identity.
    pub request_id: String,
    /// Work identity.
    pub work_id: String,
    /// Run identity.
    pub run_id: String,
    /// Attempt identity.
    pub attempt_id: String,
    /// Attempt ordinal, starting at 1.
    pub attempt_ordinal: u32,
    /// Lease identity.
    pub lease_id: String,
    /// Lease owner identity.
    pub lease_owner: String,
    /// Lease epoch.
    pub lease_epoch: u64,
    /// Lease expiry as unix milliseconds.
    pub lease_expiry_unix_ms: u64,
    /// Lease revision.
    pub lease_revision: Revision,
    /// Stable provider-request identity reused across safe retries.
    pub provider_request_id: String,
    /// Artifact policy identifier.
    pub artifact_policy: String,
    /// Retention / idempotency horizon in seconds.
    pub retention_horizon_secs: u64,
    /// Creation timestamp as unix milliseconds.
    pub created_at_unix_ms: u64,
    /// Deadline timestamp as unix milliseconds.
    pub deadline_at_unix_ms: u64,
    /// Grant class.
    pub grant_class: HostGrantClass,
    /// HMAC-SHA-256 hex over the canonical field set.
    pub spec_mac_hex: String,
}

/// A specification whose MAC has been verified with a host key.
///
/// The only constructor is [`InternalExecutionSpec::verify`]. Grants may be
/// derived only from this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSpec {
    spec: InternalExecutionSpec,
}

impl VerifiedSpec {
    /// Borrow the verified specification.
    pub fn spec(&self) -> &InternalExecutionSpec {
        &self.spec
    }
}

fn put_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Versioned, domain-separated, length-prefixed canonical bytes.
pub fn canonical_mac_bytes(domain: &str, fields: &[(&str, &[u8])]) -> Result<Vec<u8>, SpineError> {
    let mut seen = BTreeSet::new();
    for (name, _) in fields {
        if !seen.insert(*name) {
            return Err(SpineError::UnknownField);
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAC_MAGIC);
    out.extend_from_slice(&MAC_ENCODING_VERSION.to_be_bytes());
    put_lp(&mut out, domain.as_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    for (name, value) in fields {
        put_lp(&mut out, name.as_bytes());
        put_lp(&mut out, value);
    }
    Ok(out)
}

/// Host-only HMAC over canonical fields. Not exported from the SDK crate.
pub fn mac_over_fields(
    key: &MacKey,
    domain: &str,
    fields: &[(&str, &[u8])],
) -> Result<[u8; 32], SpineError> {
    hmac_tag(key, domain, fields)
}

/// Constant-time verify of [`mac_over_fields`].
pub fn verify_fields(
    key: &MacKey,
    domain: &str,
    fields: &[(&str, &[u8])],
    tag: &[u8],
) -> Result<(), SpineError> {
    let expected = hmac_tag(key, domain, fields)?;
    if tag.len() != expected.len() {
        return Err(SpineError::MacInvalid);
    }
    let mut diff = 0u8;
    for (left, right) in expected.iter().zip(tag.iter()) {
        diff |= left ^ right;
    }
    if diff == 0 {
        Ok(())
    } else {
        Err(SpineError::MacInvalid)
    }
}

fn hmac_tag(key: &MacKey, domain: &str, fields: &[(&str, &[u8])]) -> Result<[u8; 32], SpineError> {
    let bytes = canonical_mac_bytes(domain, fields)?;
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).map_err(|_| SpineError::WeakKey)?;
    mac.update(&bytes);
    let finalized = mac.finalize().into_bytes();
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&finalized);
    Ok(tag)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(text: &str) -> Result<Vec<u8>, SpineError> {
    if !text.len().is_multiple_of(2) || text.len() > 128 {
        return Err(SpineError::MacInvalid);
    }
    if !text
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(SpineError::MacInvalid);
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8, SpineError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(SpineError::MacInvalid),
    }
}

fn require_id(value: &str) -> Result<(), SpineError> {
    if value.is_empty() {
        return Err(SpineError::InvalidIdentity);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(SpineError::Utf8Ceiling);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(SpineError::InvalidIdentity);
    }
    if value.contains("..") || value.contains('/') || value.contains('\\') {
        return Err(SpineError::InvalidIdentity);
    }
    Ok(())
}

fn require_digest(value: &str) -> Result<(), SpineError> {
    if value.len() != MAX_DIGEST_HEX || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SpineError::InvalidIdentity);
    }
    Ok(())
}

fn require_field(value: &str) -> Result<(), SpineError> {
    if value.is_empty() {
        return Err(SpineError::InvalidIdentity);
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(SpineError::Utf8Ceiling);
    }
    require_id(value)
}

/// SHA-256 hex of UTF-8 bytes. Used for objective/input digests.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

/// Opaque installation-local principal derived from a host-only key.
///
/// Raw LDAP DNs, token bytes, and filesystem paths must never be stored in
/// the specification; pass them through this helper first.
pub fn opaque_principal(key: &MacKey, raw: &str) -> Result<String, SpineError> {
    if raw.is_empty() || raw.len() > MAX_FIELD_BYTES {
        return Err(SpineError::InvalidIdentity);
    }
    let tag = hmac_tag(
        key,
        "grokptah.authority.principal.v1",
        &[("raw", raw.as_bytes())],
    )?;
    Ok(format!("usr-{}", hex_encode(&tag[..16])))
}

fn capabilities_canonical(set: &[String]) -> Result<Vec<u8>, SpineError> {
    let mut sorted = set.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != set.len() {
        return Err(SpineError::DuplicateIdentity);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(sorted.len() as u32).to_be_bytes());
    let mut total = 0usize;
    for cap in &sorted {
        require_id(cap)?;
        total = total.saturating_add(cap.len());
        if total > MAX_CAPABILITY_BYTES {
            return Err(SpineError::Utf8Ceiling);
        }
        put_lp(&mut out, cap.as_bytes());
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn spec_mac_fields<'a>(
    spec: &'a InternalExecutionSpec,
    capability_bytes: &'a [u8],
    ordinal: &'a [u8],
    epoch: &'a [u8],
    expiry: &'a [u8],
    created: &'a [u8],
    deadline: &'a [u8],
    retention: &'a [u8],
    bounds_prompt: &'a [u8],
    bounds_rounds: &'a [u8],
    bounds_duration: &'a [u8],
    auth_rev: &'a [u8],
    policy_rev: &'a [u8],
    cap_rev: &'a [u8],
    cred_rev: &'a [u8],
    source_rev: &'a [u8],
    route_rev: &'a [u8],
    lease_rev: &'a [u8],
) -> [(&'a str, &'a [u8]); 38] {
    [
        ("agent", spec.agent.as_bytes()),
        ("artifact_policy", spec.artifact_policy.as_bytes()),
        ("attempt_id", spec.attempt_id.as_bytes()),
        ("attempt_ordinal", ordinal),
        ("auth_revision", auth_rev),
        ("bounds.max_duration_ms", bounds_duration),
        ("bounds.max_prompt_bytes", bounds_prompt),
        ("bounds.max_rounds", bounds_rounds),
        ("capability_revision", cap_rev),
        ("capability_set", capability_bytes),
        ("created_at_unix_ms", created),
        ("credential_id", spec.credential_id.as_bytes()),
        ("credential_revision", cred_rev),
        ("deadline_at_unix_ms", deadline),
        ("effort", spec.effort.as_bytes()),
        ("endpoint_fingerprint", spec.endpoint_fingerprint.as_bytes()),
        ("grant_class", spec.grant_class.label().as_bytes()),
        ("input_digest", spec.input_digest.as_bytes()),
        ("lease_epoch", epoch),
        ("lease_expiry_unix_ms", expiry),
        ("lease_id", spec.lease_id.as_bytes()),
        ("lease_owner", spec.lease_owner.as_bytes()),
        ("lease_revision", lease_rev),
        ("model", spec.model.as_bytes()),
        ("objective_digest", spec.objective_digest.as_bytes()),
        ("policy_revision", policy_rev),
        ("principal", spec.principal.as_bytes()),
        ("project", spec.project.as_bytes()),
        ("provider", spec.provider.as_bytes()),
        ("provider_profile", spec.provider_profile.as_bytes()),
        ("provider_request_id", spec.provider_request_id.as_bytes()),
        ("request_id", spec.request_id.as_bytes()),
        ("retention_horizon_secs", retention),
        ("route_revision", route_rev),
        ("run_id", spec.run_id.as_bytes()),
        ("session", spec.session.as_bytes()),
        ("source_revision", source_rev),
        ("tenant", spec.tenant.as_bytes()),
        // work_id and workspace fields appended via a second slice in seal()
    ]
}

impl InternalExecutionSpec {
    fn validate_fields(&self) -> Result<(), SpineError> {
        self.bounds.validate()?;
        for field in [
            self.principal.as_str(),
            self.tenant.as_str(),
            self.project.as_str(),
            self.workspace_id.as_str(),
            self.workspace_source_revision.as_str(),
            self.session.as_str(),
            self.agent.as_str(),
            self.provider.as_str(),
            self.provider_profile.as_str(),
            self.endpoint_fingerprint.as_str(),
            self.model.as_str(),
            self.effort.as_str(),
            self.credential_id.as_str(),
            self.request_id.as_str(),
            self.work_id.as_str(),
            self.run_id.as_str(),
            self.attempt_id.as_str(),
            self.lease_id.as_str(),
            self.lease_owner.as_str(),
            self.provider_request_id.as_str(),
            self.artifact_policy.as_str(),
        ] {
            require_field(field)?;
        }
        require_digest(&self.objective_digest)?;
        require_digest(&self.input_digest)?;
        if self.attempt_ordinal == 0 {
            return Err(SpineError::InvalidIdentity);
        }
        if self.deadline_at_unix_ms <= self.created_at_unix_ms {
            return Err(SpineError::InvalidIdentity);
        }
        if self.retention_horizon_secs == 0 {
            return Err(SpineError::BoundsOmitted);
        }
        if self.grant_class == HostGrantClass::HelpAnswer {
            return Err(SpineError::HelpCannotCreateDurable);
        }
        let mut values = [
            self.request_id.as_str(),
            self.work_id.as_str(),
            self.run_id.as_str(),
            self.attempt_id.as_str(),
            self.lease_id.as_str(),
            self.provider_request_id.as_str(),
        ];
        values.sort_unstable();
        for pair in values.windows(2) {
            if pair[0] == pair[1] {
                return Err(SpineError::DuplicateIdentity);
            }
        }
        Ok(())
    }

    fn mac_tag(&self, key: &MacKey) -> Result<[u8; 32], SpineError> {
        let capability_bytes = capabilities_canonical(&self.capability_set)?;
        let ordinal = self.attempt_ordinal.to_be_bytes();
        let epoch = self.lease_epoch.to_be_bytes();
        let expiry = self.lease_expiry_unix_ms.to_be_bytes();
        let created = self.created_at_unix_ms.to_be_bytes();
        let deadline = self.deadline_at_unix_ms.to_be_bytes();
        let retention = self.retention_horizon_secs.to_be_bytes();
        let bounds_prompt = self.bounds.max_prompt_bytes.to_be_bytes();
        let bounds_rounds = self.bounds.max_rounds.to_be_bytes();
        let bounds_duration = self.bounds.max_duration_ms.to_be_bytes();
        let auth_rev = self.auth_revision.get().to_be_bytes();
        let policy_rev = self.policy_revision.get().to_be_bytes();
        let cap_rev = self.capability_revision.get().to_be_bytes();
        let cred_rev = self.credential_revision.get().to_be_bytes();
        let route_rev = self.route_revision.get().to_be_bytes();
        let source_counter = self.source_revision.get().to_be_bytes();
        let lease_rev = self.lease_revision.get().to_be_bytes();
        let head = spec_mac_fields(
            self,
            &capability_bytes,
            &ordinal,
            &epoch,
            &expiry,
            &created,
            &deadline,
            &retention,
            &bounds_prompt,
            &bounds_rounds,
            &bounds_duration,
            &auth_rev,
            &policy_rev,
            &cap_rev,
            &cred_rev,
            &source_counter,
            &route_rev,
            &lease_rev,
        );
        let mut fields: Vec<(&str, &[u8])> = head.to_vec();
        fields.push(("work_id", self.work_id.as_bytes()));
        fields.push(("workspace_id", self.workspace_id.as_bytes()));
        fields.push((
            "workspace_source_revision",
            self.workspace_source_revision.as_bytes(),
        ));
        hmac_tag(key, MAC_DOMAIN_SPEC, &fields)
    }

    /// Seal this specification with a host-only key.
    pub fn seal(mut self, key: &MacKey) -> Result<Self, SpineError> {
        self.validate_fields()?;
        let tag = self.mac_tag(key)?;
        self.spec_mac_hex = hex_encode(&tag);
        Ok(self)
    }

    /// Constant-time MAC verification. Returns a grant-capable verified spec.
    pub fn verify(&self, key: &MacKey) -> Result<VerifiedSpec, SpineError> {
        self.validate_fields()?;
        let expected = self.mac_tag(key)?;
        let observed = hex_decode(&self.spec_mac_hex)?;
        if observed.len() != expected.len() {
            return Err(SpineError::MacInvalid);
        }
        let mut diff = 0u8;
        for (left, right) in expected.iter().zip(observed.iter()) {
            diff |= left ^ right;
        }
        if diff != 0 {
            return Err(SpineError::MacInvalid);
        }
        Ok(VerifiedSpec { spec: self.clone() })
    }

    /// Strict public projection. Never includes keys, MACs, paths, or payloads.
    pub fn project_public(
        &self,
        lifecycle: PublicExecutionLifecycle,
        send_state: PublicSendState,
    ) -> Result<PublicAuthorityProjection, SpineError> {
        let projection = PublicAuthorityProjection {
            contract: PUBLIC_AUTHORITY_CONTRACT_VERSION.into(),
            schema_version: PUBLIC_AUTHORITY_SCHEMA_VERSION,
            grant_class: self.grant_class.as_public(),
            lifecycle,
            send_state,
            identities: vec![
                PublicIdentity {
                    class: PublicIdentityClass::Request,
                    value: self.request_id.clone(),
                },
                PublicIdentity {
                    class: PublicIdentityClass::Work,
                    value: self.work_id.clone(),
                },
                PublicIdentity {
                    class: PublicIdentityClass::Run,
                    value: self.run_id.clone(),
                },
                PublicIdentity {
                    class: PublicIdentityClass::Attempt,
                    value: self.attempt_id.clone(),
                },
                PublicIdentity {
                    class: PublicIdentityClass::Lease,
                    value: self.lease_id.clone(),
                },
                PublicIdentity {
                    class: PublicIdentityClass::ProviderRequest,
                    value: self.provider_request_id.clone(),
                },
            ],
            attempt_generation: self.attempt_ordinal,
            lease_generation: self.lease_epoch,
            principal: Some(self.principal.clone()),
            tenant: Some(self.tenant.clone()),
            project: Some(self.project.clone()),
            workspace: self.workspace_id.clone(),
            session: self.session.clone(),
            agent: Some(self.agent.clone()),
            provider: self.provider.clone(),
            model: self.model.clone(),
            effort: Some(self.effort.clone()),
            heartbeat_unix_ms: None,
            revisions: PublicRevisionSet {
                auth: self.auth_revision.get(),
                policy: self.policy_revision.get(),
                capability: self.capability_revision.get(),
                credential: self.credential_revision.get(),
                source: self.source_revision.get(),
                route: self.route_revision.get(),
            },
            result_revision: None,
            progress: None,
            cancellation_requested: false,
            reconciliation_required: false,
            artifacts: Vec::new(),
            error_code: None,
            reason_code: None,
            promotion_ready: false,
        };
        projection
            .validate()
            .map_err(|_| SpineError::InvalidIdentity)?;
        Ok(projection)
    }
}

/// Derive a privileged grant from a verified specification only.
pub fn derive_grant(verified: &VerifiedSpec) -> Result<HostGrant, SpineError> {
    let spec = verified.spec();
    match spec.grant_class {
        HostGrantClass::ProviderRun => Ok(HostGrant::ProviderRun {
            run_id: spec.run_id.clone(),
            work_id: spec.work_id.clone(),
        }),
        HostGrantClass::ExternalWorkerAction => Ok(HostGrant::ExternalWorkerAction {
            provider_request_id: spec.provider_request_id.clone(),
        }),
        HostGrantClass::ReadOnlyDocument => Ok(HostGrant::ReadOnlyDocument {
            request_id: spec.request_id.clone(),
        }),
        HostGrantClass::HelpAnswer => Err(SpineError::HelpCannotCreateDurable),
    }
}

/// Live host revisions compared against the sealed specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRevisions {
    /// Auth revision currently in force.
    pub auth: Revision,
    /// Policy revision currently in force.
    pub policy: Revision,
    /// Capability revision currently in force.
    pub capability: Revision,
    /// Credential revision currently in force.
    pub credential: Revision,
    /// Route revision currently in force.
    pub route: Revision,
    /// Source revision currently in force.
    pub source: Revision,
}

impl LiveRevisions {
    /// Fail closed on any drift.
    pub fn check(self, spec: &InternalExecutionSpec) -> Result<(), SpineError> {
        self.auth.require_current(spec.auth_revision)?;
        self.policy.require_current(spec.policy_revision)?;
        self.capability.require_current(spec.capability_revision)?;
        self.credential.require_current(spec.credential_revision)?;
        self.route.require_current(spec.route_revision)?;
        self.source.require_current(spec.source_revision)?;
        Ok(())
    }
}

impl Default for LiveRevisions {
    fn default() -> Self {
        Self {
            auth: Revision::new(1),
            policy: Revision::new(1),
            capability: Revision::new(1),
            credential: Revision::new(1),
            route: Revision::new(1),
            source: Revision::new(1),
        }
    }
}

/// Bounds JSON must include every required field and deny unknown keys.
pub fn parse_bounds_json(value: &str) -> Result<AcceptedBounds, SpineError> {
    serde_json::from_str::<AcceptedBounds>(value).map_err(|_| SpineError::UnknownField)
}

/// Unsigned spec used by tests before sealing. Not a public SDK constructor.
pub fn unsigned_provider_spec(suffix: &str, input: &str) -> InternalExecutionSpec {
    let now = 1_700_000_000_000u64;
    InternalExecutionSpec {
        principal: format!("usr-principal-{suffix}"),
        tenant: format!("tenant-{suffix}"),
        project: format!("project-{suffix}"),
        workspace_id: format!("workspace-{suffix}"),
        workspace_source_revision: "0123456789abcdef0123456789abcdef01234567".into(),
        session: format!("session-{suffix}"),
        agent: format!("agent-{suffix}"),
        objective_digest: sha256_hex(input.as_bytes()),
        input_digest: sha256_hex(input.as_bytes()),
        provider: "xai".into(),
        provider_profile: "xai-default".into(),
        endpoint_fingerprint: format!("ep-{suffix}"),
        model: "grok-4".into(),
        effort: "high".into(),
        bounds: AcceptedBounds {
            max_prompt_bytes: 4096,
            max_rounds: 4,
            max_duration_ms: 60_000,
        },
        capability_set: vec!["run.execute".into()],
        policy_revision: Revision::new(1),
        capability_revision: Revision::new(1),
        auth_revision: Revision::new(1),
        credential_id: format!("cred-{suffix}"),
        credential_revision: Revision::new(1),
        route_revision: Revision::new(1),
        source_revision: Revision::new(1),
        request_id: format!("req-{suffix}"),
        work_id: format!("work-{suffix}"),
        run_id: format!("run-{suffix}"),
        attempt_id: format!("att-{suffix}"),
        attempt_ordinal: 1,
        lease_id: format!("lease-{suffix}"),
        lease_owner: format!("owner-{suffix}"),
        lease_epoch: 1,
        lease_expiry_unix_ms: now + 60_000,
        lease_revision: Revision::new(1),
        provider_request_id: format!("preq-{suffix}"),
        artifact_policy: "isolated-diff-v1".into(),
        retention_horizon_secs: 86_400,
        created_at_unix_ms: now,
        deadline_at_unix_ms: now + 60_000,
        grant_class: HostGrantClass::ProviderRun,
        spec_mac_hex: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> MacKey {
        MacKey::from_bytes(&[0x5a; 32]).unwrap()
    }

    #[test]
    fn mac_domain_key_order_and_length_are_ambiguous_only_when_canonical() {
        let key = test_key();
        let other = MacKey::from_bytes(&[0x22; 32]).unwrap();
        let a = mac_over_fields(&key, MAC_DOMAIN_SPEC, &[("a", b"ab"), ("b", b"c")]).unwrap();
        let b = mac_over_fields(&key, MAC_DOMAIN_SPEC, &[("a", b"a"), ("b", b"bc")]).unwrap();
        assert_ne!(a, b, "length-prefix must prevent concatenation ambiguity");
        let reordered =
            mac_over_fields(&key, MAC_DOMAIN_SPEC, &[("b", b"c"), ("a", b"ab")]).unwrap();
        assert_ne!(a, reordered);
        let other_domain = mac_over_fields(
            &key,
            "grokptah.authority.other.v1",
            &[("a", b"ab"), ("b", b"c")],
        )
        .unwrap();
        assert_ne!(a, other_domain);
        let other_key =
            mac_over_fields(&other, MAC_DOMAIN_SPEC, &[("a", b"ab"), ("b", b"c")]).unwrap();
        assert_ne!(a, other_key);
        assert_eq!(
            canonical_mac_bytes(MAC_DOMAIN_SPEC, &[("a", b"x"), ("a", b"y")]).unwrap_err(),
            SpineError::UnknownField
        );
    }

    #[test]
    fn weak_keys_and_debug_are_secret_free() {
        assert_eq!(
            MacKey::from_bytes(&[0x11; 16]).unwrap_err(),
            SpineError::WeakKey
        );
        let key = test_key();
        let debug = format!("{key:?}");
        assert!(!debug.contains("5a"));
        assert!(debug.contains("256"));
    }

    #[test]
    fn agent_effort_revisions_and_bounds_are_mac_covered() {
        let key = test_key();
        let sealed = unsigned_provider_spec("mac", "intent").seal(&key).unwrap();
        sealed.verify(&key).unwrap();
        let mut mutated = sealed.clone();
        mutated.agent = "agent-other".into();
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.effort = "low".into();
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.policy_revision = Revision::new(9);
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.bounds.max_rounds = 8;
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.credential_revision = Revision::new(2);
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.route_revision = Revision::new(3);
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.lease_epoch = 99;
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
        mutated = sealed.clone();
        mutated.input_digest = sha256_hex(b"other");
        assert_eq!(mutated.verify(&key), Err(SpineError::MacInvalid));
    }

    #[test]
    fn derive_grant_requires_verified_spec() {
        let key = test_key();
        let sealed = unsigned_provider_spec("grant", "intent")
            .seal(&key)
            .unwrap();
        let verified = sealed.verify(&key).unwrap();
        assert!(matches!(
            derive_grant(&verified).unwrap(),
            HostGrant::ProviderRun { .. }
        ));
    }
}
