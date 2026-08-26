//! Host-only authenticated authority envelope.
//!
//! Public SDK types are a redacted projection. Deserialization yields
//! [`UnverifiedEnvelope`]. Only host verification produces [`VerifiedEnvelope`],
//! the sole type grant derivation accepts.

use std::fmt;
use std::path::Path;

use grokptah_agent_sdk::authority::{
    PublicAuthorityProjection, PublicGrantClass, PublicIdentity, PublicIdentityClass,
    PublicRevisionSet, PublicSendState, PUBLIC_AUTHORITY_CONTRACT_VERSION,
    PUBLIC_AUTHORITY_SCHEMA_VERSION,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

type HmacSha256 = Hmac<Sha256>;

pub const MAC_DOMAIN_ENVELOPE: &str = "grokptah.authority.envelope.v1";
pub const MAC_ENCODING_VERSION: u32 = 1;
const MAC_MAGIC: &[u8] = b"GPTA.MAC.v1";
const MIN_KEY_BYTES: usize = 32;
const MAX_FIELD_BYTES: usize = 4 * 1024;
const MAX_INTENT_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineError {
    InvalidIdentity,
    DuplicateIdentity,
    ExtraIdentity,
    MacInvalid,
    WeakKey,
    KeyAbsent,
    Utf8Ceiling,
    BoundsOmitted,
    HelpCannotCreateDurable,
    Unverified,
}

impl SpineError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIdentity => "invalid_identity",
            Self::DuplicateIdentity => "duplicate_identity",
            Self::ExtraIdentity => "extra_identity",
            Self::MacInvalid => "mac_invalid",
            Self::WeakKey => "weak_key",
            Self::KeyAbsent => "key_absent",
            Self::Utf8Ceiling => "utf8_ceiling",
            Self::BoundsOmitted => "bounds_omitted",
            Self::HelpCannotCreateDurable => "help_cannot_create_durable",
            Self::Unverified => "unverified",
        }
    }
}

/// Host-provisioned HMAC key. Zeroized on drop. Never projected.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct AuthorityKey {
    id: String,
    version: u32,
    bytes: Vec<u8>,
}

impl fmt::Debug for AuthorityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorityKey")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("bits", &(self.bytes.len().saturating_mul(8)))
            .finish()
    }
}

impl AuthorityKey {
    pub fn provision(
        id: impl Into<String>,
        version: u32,
        bytes: &[u8],
    ) -> Result<Self, SpineError> {
        if bytes.len() < MIN_KEY_BYTES {
            return Err(SpineError::WeakKey);
        }
        let id = id.into();
        if id.is_empty() || id.len() > 64 {
            return Err(SpineError::InvalidIdentity);
        }
        Ok(Self {
            id,
            version,
            bytes: bytes.to_vec(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    /// Load from the host provisioning seam. Absence fails closed.
    pub fn load_from_host() -> Result<Self, SpineError> {
        if let Ok(path) = std::env::var("GROKPTAH_AUTHORITY_KEY_FILE") {
            return load_key_file(Path::new(&path));
        }
        Err(SpineError::KeyAbsent)
    }

    /// Load an existing host key, or provision one at `path` (mode 0600).
    pub fn load_or_provision(path: &Path) -> Result<Self, SpineError> {
        if let Ok(env_path) = std::env::var("GROKPTAH_AUTHORITY_KEY_FILE") {
            return load_key_file(Path::new(&env_path));
        }
        if path.is_file() {
            return load_key_file(path);
        }
        let mut bytes = [0u8; 32];
        fill_random_bytes(&mut bytes)?;
        let key = Self::provision("host-default", 1, &bytes)?;
        write_key_file(path, &key)?;
        Ok(key)
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn fill_random_bytes(out: &mut [u8]) -> Result<(), SpineError> {
    use std::io::Read;
    let mut file = std::fs::File::open("/dev/urandom").map_err(|_| SpineError::KeyAbsent)?;
    file.read_exact(out).map_err(|_| SpineError::KeyAbsent)
}

fn write_key_file(path: &Path, key: &AuthorityKey) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| SpineError::KeyAbsent)?;
    }
    let body = format!(
        "id={}\nversion={}\nhex={}\n",
        key.id(),
        key.version(),
        hex_encode(key.as_bytes())
    );
    let tmp = path.with_extension("key.tmp");
    std::fs::write(&tmp, body).map_err(|_| SpineError::KeyAbsent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp, perm).map_err(|_| SpineError::KeyAbsent)?;
    }
    std::fs::rename(&tmp, path).map_err(|_| SpineError::KeyAbsent)
}

fn load_key_file(path: &Path) -> Result<AuthorityKey, SpineError> {
    let text = std::fs::read_to_string(path).map_err(|_| SpineError::KeyAbsent)?;
    let mut id = "host-default".to_string();
    let mut version = 1u32;
    let mut hex = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("id=") {
            id = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("version=") {
            version = rest.trim().parse().map_err(|_| SpineError::KeyAbsent)?;
        } else if let Some(rest) = line.strip_prefix("hex=") {
            hex = rest.trim().to_string();
        }
    }
    let bytes = hex_decode(&hex)?;
    AuthorityKey::provision(id, version, &bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityClass {
    Request,
    Work,
    Run,
    Attempt,
    Lease,
    ProviderRequest,
    WorkerLiveness,
    Finalization,
    Receipt,
    Tombstone,
}

impl IdentityClass {
    fn label(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Work => "work",
            Self::Run => "run",
            Self::Attempt => "attempt",
            Self::Lease => "lease",
            Self::ProviderRequest => "provider_request",
            Self::WorkerLiveness => "worker_liveness",
            Self::Finalization => "finalization",
            Self::Receipt => "receipt",
            Self::Tombstone => "tombstone",
        }
    }

    fn as_public(self) -> PublicIdentityClass {
        match self {
            Self::Request => PublicIdentityClass::Request,
            Self::Work => PublicIdentityClass::Work,
            Self::Run => PublicIdentityClass::Run,
            Self::Attempt => PublicIdentityClass::Attempt,
            Self::Lease => PublicIdentityClass::Lease,
            Self::ProviderRequest => PublicIdentityClass::ProviderRequest,
            Self::WorkerLiveness => PublicIdentityClass::WorkerLiveness,
            Self::Finalization => PublicIdentityClass::Finalization,
            Self::Receipt => PublicIdentityClass::Receipt,
            Self::Tombstone => PublicIdentityClass::Tombstone,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassifiedId {
    pub class: IdentityClass,
    pub value: String,
}

impl ClassifiedId {
    pub fn new(class: IdentityClass, value: impl Into<String>) -> Result<Self, SpineError> {
        let value = value.into();
        validate_id(&value)?;
        Ok(Self { class, value })
    }
}

fn validate_id(value: &str) -> Result<(), SpineError> {
    if value.is_empty() {
        return Err(SpineError::InvalidIdentity);
    }
    if value.len() > 128 {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGrantClass {
    ProviderRun,
    ExternalWorkerAction,
    ReadOnlyDocument,
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

    fn required_identities(self) -> &'static [IdentityClass] {
        match self {
            Self::ProviderRun => &[
                IdentityClass::Request,
                IdentityClass::Work,
                IdentityClass::Run,
                IdentityClass::Attempt,
                IdentityClass::Lease,
                IdentityClass::ProviderRequest,
            ],
            Self::ExternalWorkerAction => &[IdentityClass::Request, IdentityClass::WorkerLiveness],
            Self::ReadOnlyDocument | Self::HelpAnswer => &[IdentityClass::Request],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionBounds {
    pub max_duration_ms: u64,
    pub max_rounds: u32,
    pub max_tokens: u64,
    pub max_cost_cents: u64,
    pub max_tools: u32,
}

impl ExecutionBounds {
    pub fn validate(&self) -> Result<(), SpineError> {
        if self.max_duration_ms == 0 || self.max_rounds == 0 {
            return Err(SpineError::BoundsOmitted);
        }
        Ok(())
    }
}

/// Wire envelope. Deserialization produces this unverified type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnverifiedEnvelope {
    pub principal: String,
    pub tenant: String,
    pub project: String,
    pub workspace: String,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub provider: String,
    pub profile: String,
    pub endpoint_fingerprint: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub auth_revision: u64,
    pub policy_revision: u64,
    pub capability_revision: u64,
    pub credential_revision: u64,
    pub source_revision: u64,
    pub bounds: ExecutionBounds,
    pub identities: Vec<ClassifiedId>,
    pub grant_class: HostGrantClass,
    pub intent_digest: String,
    pub expires_unix: i64,
    pub key_id: String,
    pub key_version: u32,
    pub envelope_mac_hex: String,
}

/// Host-verified envelope. Not constructible from deserialized bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEnvelope {
    inner: UnverifiedEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostGrant {
    ProviderRun { run: ClassifiedId },
    ExternalWorkerAction { liveness: ClassifiedId },
    ReadOnlyDocument { request: ClassifiedId },
    HelpAnswer { request: ClassifiedId },
}

fn put_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn canonical_mac_bytes(fields: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAC_MAGIC);
    out.extend_from_slice(&MAC_ENCODING_VERSION.to_be_bytes());
    put_lp(&mut out, MAC_DOMAIN_ENVELOPE.as_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    for (name, value) in fields {
        put_lp(&mut out, name.as_bytes());
        put_lp(&mut out, value);
    }
    out
}

fn hmac_tag(key: &AuthorityKey, fields: &[(&str, &[u8])]) -> Result<[u8; 32], SpineError> {
    let mac = hmac_context(key, fields)?;
    let finalized = mac.finalize().into_bytes();
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&finalized);
    Ok(tag)
}

fn hmac_verify(key: &AuthorityKey, fields: &[(&str, &[u8])], tag: &[u8]) -> Result<(), SpineError> {
    hmac_context(key, fields)?
        .verify_slice(tag)
        .map_err(|_| SpineError::MacInvalid)
}

fn hmac_context(key: &AuthorityKey, fields: &[(&str, &[u8])]) -> Result<HmacSha256, SpineError> {
    let bytes = canonical_mac_bytes(fields);
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).map_err(|_| SpineError::WeakKey)?;
    mac.update(&bytes);
    Ok(mac)
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
    if !text.len().is_multiple_of(2) || text.is_empty() || text.len() > 256 {
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

fn require_field(value: &str) -> Result<(), SpineError> {
    if value.is_empty() {
        return Err(SpineError::InvalidIdentity);
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(SpineError::Utf8Ceiling);
    }
    validate_id(value)
}

fn identities_canonical(ids: &[ClassifiedId]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(ids.len() as u32).to_be_bytes());
    for id in ids {
        put_lp(&mut out, id.class.label().as_bytes());
        put_lp(&mut out, id.value.as_bytes());
    }
    out
}

fn with_envelope_fields<T>(
    envelope: &UnverifiedEnvelope,
    f: impl FnOnce(&[(&str, &[u8])]) -> T,
) -> T {
    let identities = identities_canonical(&envelope.identities);
    let duration = envelope.bounds.max_duration_ms.to_be_bytes();
    let rounds = envelope.bounds.max_rounds.to_be_bytes();
    let tokens = envelope.bounds.max_tokens.to_be_bytes();
    let cost = envelope.bounds.max_cost_cents.to_be_bytes();
    let tools = envelope.bounds.max_tools.to_be_bytes();
    let auth = envelope.auth_revision.to_be_bytes();
    let policy = envelope.policy_revision.to_be_bytes();
    let capability = envelope.capability_revision.to_be_bytes();
    let credential = envelope.credential_revision.to_be_bytes();
    let source = envelope.source_revision.to_be_bytes();
    let expires = envelope.expires_unix.to_be_bytes();
    let key_version = envelope.key_version.to_be_bytes();
    f(&[
        ("principal", envelope.principal.as_bytes()),
        ("tenant", envelope.tenant.as_bytes()),
        ("project", envelope.project.as_bytes()),
        ("workspace", envelope.workspace.as_bytes()),
        ("session", envelope.session.as_bytes()),
        ("agent", envelope.agent.as_deref().unwrap_or("").as_bytes()),
        ("provider", envelope.provider.as_bytes()),
        ("profile", envelope.profile.as_bytes()),
        ("endpoint", envelope.endpoint_fingerprint.as_bytes()),
        ("model", envelope.model.as_bytes()),
        (
            "effort",
            envelope.effort.as_deref().unwrap_or("").as_bytes(),
        ),
        ("auth_rev", &auth),
        ("policy_rev", &policy),
        ("capability_rev", &capability),
        ("credential_rev", &credential),
        ("source_rev", &source),
        ("max_duration_ms", &duration),
        ("max_rounds", &rounds),
        ("max_tokens", &tokens),
        ("max_cost_cents", &cost),
        ("max_tools", &tools),
        ("grant", envelope.grant_class.label().as_bytes()),
        ("identities", identities.as_slice()),
        ("intent_digest", envelope.intent_digest.as_bytes()),
        ("expires_unix", &expires),
        ("key_id", envelope.key_id.as_bytes()),
        ("key_version", &key_version),
        ("encoding", b"1"),
        ("domain", MAC_DOMAIN_ENVELOPE.as_bytes()),
    ])
}

fn mac_envelope(key: &AuthorityKey, envelope: &UnverifiedEnvelope) -> Result<[u8; 32], SpineError> {
    with_envelope_fields(envelope, |fields| hmac_tag(key, fields))
}

fn verify_envelope_mac(
    key: &AuthorityKey,
    envelope: &UnverifiedEnvelope,
    tag: &[u8],
) -> Result<(), SpineError> {
    with_envelope_fields(envelope, |fields| hmac_verify(key, fields, tag))
}

fn require_exact_identities(
    ids: &[ClassifiedId],
    required: &[IdentityClass],
) -> Result<(), SpineError> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id.class) {
            return Err(SpineError::DuplicateIdentity);
        }
        if !required.contains(&id.class) {
            return Err(SpineError::ExtraIdentity);
        }
    }
    for class in required {
        if !ids.iter().any(|id| id.class == *class) {
            return Err(SpineError::InvalidIdentity);
        }
    }
    Ok(())
}

impl UnverifiedEnvelope {
    pub fn seal(mut self, key: &AuthorityKey) -> Result<VerifiedEnvelope, SpineError> {
        if self.intent_digest.len() > MAX_INTENT_BYTES {
            return Err(SpineError::Utf8Ceiling);
        }
        self.bounds.validate()?;
        for field in [
            self.principal.as_str(),
            self.tenant.as_str(),
            self.project.as_str(),
            self.workspace.as_str(),
            self.session.as_str(),
            self.provider.as_str(),
            self.profile.as_str(),
            self.endpoint_fingerprint.as_str(),
            self.model.as_str(),
            self.intent_digest.as_str(),
        ] {
            require_field(field)?;
        }
        if let Some(agent) = &self.agent {
            require_field(agent)?;
        }
        if let Some(effort) = &self.effort {
            require_field(effort)?;
        }
        require_exact_identities(&self.identities, self.grant_class.required_identities())?;
        self.key_id = key.id().to_string();
        self.key_version = key.version();
        let tag = mac_envelope(key, &self)?;
        self.envelope_mac_hex = hex_encode(&tag);
        Ok(VerifiedEnvelope { inner: self })
    }

    pub fn verify(self, key: &AuthorityKey) -> Result<VerifiedEnvelope, SpineError> {
        if self.key_id != key.id() || self.key_version != key.version() {
            return Err(SpineError::MacInvalid);
        }
        let tag = hex_decode(&self.envelope_mac_hex)?;
        verify_envelope_mac(key, &self, &tag)?;
        require_exact_identities(&self.identities, self.grant_class.required_identities())?;
        Ok(VerifiedEnvelope { inner: self })
    }
}

impl VerifiedEnvelope {
    pub fn inner(&self) -> &UnverifiedEnvelope {
        &self.inner
    }

    pub fn grant(&self) -> Result<HostGrant, SpineError> {
        derive_grant(self)
    }

    pub fn project_public(
        &self,
        send_state: PublicSendState,
    ) -> Result<PublicAuthorityProjection, SpineError> {
        let projection = PublicAuthorityProjection {
            contract: PUBLIC_AUTHORITY_CONTRACT_VERSION.into(),
            schema_version: PUBLIC_AUTHORITY_SCHEMA_VERSION,
            grant_class: self.inner.grant_class.as_public(),
            send_state,
            identities: self
                .inner
                .identities
                .iter()
                .map(|id| PublicIdentity {
                    class: id.class.as_public(),
                    value: id.value.clone(),
                })
                .collect(),
            principal: Some(self.inner.principal.clone()),
            tenant: Some(self.inner.tenant.clone()),
            project: Some(self.inner.project.clone()),
            workspace: self.inner.workspace.clone(),
            session: self.inner.session.clone(),
            agent: self.inner.agent.clone(),
            provider: self.inner.provider.clone(),
            model: self.inner.model.clone(),
            effort: self.inner.effort.clone(),
            revisions: PublicRevisionSet {
                auth: self.inner.auth_revision,
                policy: self.inner.policy_revision,
                capability: self.inner.capability_revision,
                credential: self.inner.credential_revision,
                source: self.inner.source_revision,
            },
        };
        projection
            .validate()
            .map_err(|_| SpineError::InvalidIdentity)?;
        Ok(projection)
    }
}

pub struct ProviderRunMint<'a> {
    pub principal: &'a str,
    pub tenant: &'a str,
    pub project: &'a str,
    pub workspace: &'a str,
    pub session: &'a str,
    pub agent: Option<&'a str>,
    pub provider: &'a str,
    pub profile: &'a str,
    pub endpoint_fingerprint: &'a str,
    pub model: &'a str,
    pub effort: Option<&'a str>,
    pub identities: Vec<ClassifiedId>,
    pub intent_digest: &'a str,
    pub bounds: ExecutionBounds,
}

pub fn mint_provider_run_envelope(
    key: &AuthorityKey,
    mint: ProviderRunMint<'_>,
) -> Result<VerifiedEnvelope, SpineError> {
    UnverifiedEnvelope {
        principal: mint.principal.into(),
        tenant: mint.tenant.into(),
        project: mint.project.into(),
        workspace: mint.workspace.into(),
        session: mint.session.into(),
        agent: mint.agent.map(str::to_string),
        provider: mint.provider.into(),
        profile: mint.profile.into(),
        endpoint_fingerprint: mint.endpoint_fingerprint.into(),
        model: mint.model.into(),
        effort: mint.effort.map(str::to_string),
        auth_revision: 1,
        policy_revision: 1,
        capability_revision: 1,
        credential_revision: 1,
        source_revision: 1,
        bounds: mint.bounds,
        identities: mint.identities,
        grant_class: HostGrantClass::ProviderRun,
        intent_digest: mint.intent_digest.into(),
        expires_unix: chrono::Utc::now().timestamp().saturating_add(86_400),
        key_id: String::new(),
        key_version: 0,
        envelope_mac_hex: String::new(),
    }
    .seal(key)
}

pub fn public_send_state(
    state: grokptah_agent_sdk::attempt::SendState,
) -> grokptah_agent_sdk::authority::PublicSendState {
    use grokptah_agent_sdk::attempt::SendState;
    match state {
        SendState::KnownNotSent => grokptah_agent_sdk::authority::PublicSendState::KnownNotSent,
        SendState::Sending => grokptah_agent_sdk::authority::PublicSendState::Sending,
        SendState::Sent => grokptah_agent_sdk::authority::PublicSendState::Sent,
        SendState::Uncertain => grokptah_agent_sdk::authority::PublicSendState::Uncertain,
        SendState::Responding => grokptah_agent_sdk::authority::PublicSendState::Responding,
        SendState::Settled => grokptah_agent_sdk::authority::PublicSendState::Settled,
    }
}

pub(crate) fn derive_grant(envelope: &VerifiedEnvelope) -> Result<HostGrant, SpineError> {
    match envelope.inner.grant_class {
        HostGrantClass::ProviderRun => {
            let run = envelope
                .inner
                .identities
                .iter()
                .find(|id| id.class == IdentityClass::Run)
                .cloned()
                .ok_or(SpineError::InvalidIdentity)?;
            Ok(HostGrant::ProviderRun { run })
        }
        HostGrantClass::ExternalWorkerAction => {
            let liveness = envelope
                .inner
                .identities
                .iter()
                .find(|id| id.class == IdentityClass::WorkerLiveness)
                .cloned()
                .ok_or(SpineError::InvalidIdentity)?;
            Ok(HostGrant::ExternalWorkerAction { liveness })
        }
        HostGrantClass::ReadOnlyDocument => {
            let request = envelope
                .inner
                .identities
                .iter()
                .find(|id| id.class == IdentityClass::Request)
                .cloned()
                .ok_or(SpineError::InvalidIdentity)?;
            Ok(HostGrant::ReadOnlyDocument { request })
        }
        HostGrantClass::HelpAnswer => {
            if envelope.inner.identities.iter().any(|id| {
                matches!(
                    id.class,
                    IdentityClass::Run
                        | IdentityClass::Work
                        | IdentityClass::Attempt
                        | IdentityClass::Lease
                )
            }) {
                return Err(SpineError::HelpCannotCreateDurable);
            }
            let request = envelope
                .inner
                .identities
                .iter()
                .find(|id| id.class == IdentityClass::Request)
                .cloned()
                .ok_or(SpineError::InvalidIdentity)?;
            Ok(HostGrant::HelpAnswer { request })
        }
    }
}

#[cfg(test)]
pub(crate) fn test_authority_key() -> AuthorityKey {
    AuthorityKey::provision("test-key", 1, &[0x11; 32]).expect("32-byte test key")
}

#[cfg(test)]
fn unsigned_fixture(
    grant_class: HostGrantClass,
    identities: Vec<ClassifiedId>,
) -> UnverifiedEnvelope {
    UnverifiedEnvelope {
        principal: "principal-1".into(),
        tenant: "tenant-1".into(),
        project: "project-1".into(),
        workspace: "workspace-1".into(),
        session: "session-1".into(),
        agent: Some("agent-1".into()),
        provider: "xai".into(),
        profile: "xai".into(),
        endpoint_fingerprint: "ep-fp-1".into(),
        model: "grok-4".into(),
        effort: Some("high".into()),
        auth_revision: 1,
        policy_revision: 2,
        capability_revision: 3,
        credential_revision: 4,
        source_revision: 5,
        bounds: ExecutionBounds {
            max_duration_ms: 60_000,
            max_rounds: 8,
            max_tokens: 32_000,
            max_cost_cents: 50,
            max_tools: 16,
        },
        identities,
        grant_class,
        intent_digest: "sha256:abc".into(),
        expires_unix: 1_900_000_000,
        key_id: String::new(),
        key_version: 0,
        envelope_mac_hex: String::new(),
    }
}

#[cfg(test)]
fn provider_run_ids() -> Vec<ClassifiedId> {
    vec![
        ClassifiedId::new(IdentityClass::Request, "req-1").unwrap(),
        ClassifiedId::new(IdentityClass::Work, "work-1").unwrap(),
        ClassifiedId::new(IdentityClass::Run, "run-1").unwrap(),
        ClassifiedId::new(IdentityClass::Attempt, "att-1").unwrap(),
        ClassifiedId::new(IdentityClass::Lease, "lease-1").unwrap(),
        ClassifiedId::new(IdentityClass::ProviderRequest, "prq-1").unwrap(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_absence_fails_closed() {
        let previous = std::env::var_os("GROKPTAH_AUTHORITY_KEY_FILE");
        std::env::remove_var("GROKPTAH_AUTHORITY_KEY_FILE");
        let result = AuthorityKey::load_from_host();
        match previous {
            Some(value) => std::env::set_var("GROKPTAH_AUTHORITY_KEY_FILE", value),
            None => std::env::remove_var("GROKPTAH_AUTHORITY_KEY_FILE"),
        }
        assert_eq!(result.err().map(|e| e.as_str()), Some("key_absent"));
    }

    #[test]
    fn deserialized_envelope_cannot_derive_a_grant() {
        let key = test_authority_key();
        let verified = unsigned_fixture(HostGrantClass::ProviderRun, provider_run_ids())
            .seal(&key)
            .unwrap();
        let json = serde_json::to_string(&verified.inner).unwrap();
        let unverified: UnverifiedEnvelope = serde_json::from_str(&json).unwrap();
        // Grant derivation is typed on VerifiedEnvelope only.
        let re_verified = unverified.clone().verify(&key).unwrap();
        assert!(derive_grant(&re_verified).is_ok());
        assert!(unverified.verify(&test_authority_key()).is_ok());
    }

    #[test]
    fn extra_identity_class_is_rejected() {
        let key = test_authority_key();
        let mut ids = provider_run_ids();
        ids.push(ClassifiedId::new(IdentityClass::Tombstone, "tomb-1").unwrap());
        let err = unsigned_fixture(HostGrantClass::ProviderRun, ids)
            .seal(&key)
            .err()
            .unwrap();
        assert_eq!(err, SpineError::ExtraIdentity);
    }

    #[test]
    fn one_bit_flip_of_every_mac_field_fails_closed() {
        let key = test_authority_key();
        let verified = unsigned_fixture(HostGrantClass::ProviderRun, provider_run_ids())
            .seal(&key)
            .unwrap();
        let mut raw = verified.inner.clone();
        raw.principal = "principal-2".into();
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.agent = Some("agent-2".into());
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.effort = Some("low".into());
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.auth_revision = 99;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.policy_revision = 99;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.capability_revision = 99;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.credential_revision = 99;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.source_revision = 99;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.bounds.max_duration_ms = 1;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.bounds.max_rounds = 1;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.bounds.max_tokens = 1;
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.model = "grok-3".into();
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
        raw = verified.inner.clone();
        raw.envelope_mac_hex = {
            let mut bytes = hex_decode(&raw.envelope_mac_hex).unwrap();
            bytes[0] ^= 1;
            hex_encode(&bytes)
        };
        assert_eq!(raw.verify(&key), Err(SpineError::MacInvalid));
    }

    #[test]
    fn public_projection_is_secret_free() {
        let key = test_authority_key();
        let verified = unsigned_fixture(HostGrantClass::ProviderRun, provider_run_ids())
            .seal(&key)
            .unwrap();
        let public = verified
            .project_public(PublicSendState::KnownNotSent)
            .unwrap();
        let encoded = serde_json::to_string(&public).unwrap();
        assert!(!encoded.contains("mac"));
        assert!(!encoded.contains("hmac"));
        assert!(!encoded.contains(&verified.inner.envelope_mac_hex));
        assert!(!encoded.contains("/Users/"));
    }

    #[test]
    fn help_cannot_carry_run_identities() {
        let ids = vec![
            ClassifiedId::new(IdentityClass::Request, "req-h").unwrap(),
            ClassifiedId::new(IdentityClass::Run, "run-h").unwrap(),
        ];
        let err = unsigned_fixture(HostGrantClass::HelpAnswer, ids)
            .seal(&test_authority_key())
            .err()
            .unwrap();
        assert_eq!(err, SpineError::ExtraIdentity);
    }
}
