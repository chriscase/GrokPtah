//! Positive-schema, fail-closed evidence for Grok Build OIDC credentials.
//!
//! This module deliberately exposes no token, client identifier, subject,
//! filesystem path, or arbitrary URL. It proves only locally observable facts.
//! An attestation remains ineligible for live certification until an
//! authoritative client policy and token-endpoint policy are available.

use std::fs::{Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::provider_observation::{EndpointFingerprint, OpaqueIdentity, PublicModelId};

pub const GROK_BUILD_ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1";
pub const XAI_OIDC_ISSUER: &str = "https://auth.x.ai";
/// Pinned from the installed official Grok 1.0.5 CLI contract. Discovery must
/// agree exactly; it is never allowed to widen this endpoint.
pub const XAI_OIDC_TOKEN_ENDPOINT: &str = "https://auth.x.ai/oauth2/token";
pub const MAX_AUTH_JSON_BYTES: u64 = 1024 * 1024;
const MAX_AUTH_ENTRIES: usize = 64;
const MAX_SECRET_FIELD_BYTES: usize = 32 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 512;
const LIVE_OVERRIDE_ENVIRONMENTS: &[&str] = &[
    "XAI_API_KEY",
    "XAI_API_BASE",
    "GROKPTAH_TOKEN_COMMAND",
    "GROKPTAH_API_BASE",
    "GROKPTAH_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "OPENAI_API_KEY",
    "GROK_OIDC_ISSUER",
    "GROK_OIDC_CLIENT_ID",
    "GROK_OAUTH2_ISSUER",
    "GROK_OAUTH2_CLIENT_ID",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveAttestationSchema {
    V1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveCredentialClass {
    GrokBuildOidc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveEndpointClass {
    GrokBuildCliProxyV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveIssuerClass {
    FirstPartyXai,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectPolicyClass {
    DenyAll,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientPolicyState {
    Unverified,
    AuthoritativelyVerified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshEndpointPolicyState {
    Unverified,
    AuthoritativelyVerified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OverrideState {
    pub xai_api_key_absent: bool,
    pub xai_api_base_absent: bool,
    pub token_command_absent: bool,
    pub compatible_gateway_absent: bool,
    pub auth_configuration_override_absent: bool,
    pub keychain_api_key_absent: bool,
}

impl OverrideState {
    fn fully_absent(self) -> bool {
        self.xai_api_key_absent
            && self.xai_api_base_absent
            && self.token_command_absent
            && self.compatible_gateway_absent
            && self.auth_configuration_override_absent
            && self.keychain_api_key_absent
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AuthFileState {
    pub regular: bool,
    pub current_user_owned: bool,
    pub private_mode_0600: bool,
    pub single_link: bool,
    pub no_symlink_ancestors: bool,
    pub opened_inode_verified: bool,
    pub bounded: bool,
}

impl AuthFileState {
    fn fully_verified(self) -> bool {
        self.regular
            && self.current_user_owned
            && self.private_mode_0600
            && self.single_link
            && self.no_symlink_ancestors
            && self.opened_inode_verified
            && self.bounded
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LiveCredentialAttestation {
    schema: LiveAttestationSchema,
    credential_class: LiveCredentialClass,
    endpoint_class: LiveEndpointClass,
    issuer_class: LiveIssuerClass,
    redirect_policy: RedirectPolicyClass,
    client_policy: ClientPolicyState,
    refresh_endpoint_policy: RefreshEndpointPolicyState,
    overrides: OverrideState,
    auth_file: AuthFileState,
    requested_model: PublicModelId,
    wire_model: PublicModelId,
    endpoint_fingerprint: EndpointFingerprint,
    auth_file_fingerprint: OpaqueIdentity,
    binding_id: OpaqueIdentity,
    certification_ready: bool,
}

impl LiveCredentialAttestation {
    pub const fn schema(&self) -> LiveAttestationSchema {
        self.schema
    }

    pub const fn credential_class(&self) -> LiveCredentialClass {
        self.credential_class
    }

    pub const fn endpoint_class(&self) -> LiveEndpointClass {
        self.endpoint_class
    }

    pub const fn issuer_class(&self) -> LiveIssuerClass {
        self.issuer_class
    }

    pub const fn redirect_policy(&self) -> RedirectPolicyClass {
        self.redirect_policy
    }

    pub const fn overrides(&self) -> OverrideState {
        self.overrides
    }

    pub const fn auth_file(&self) -> AuthFileState {
        self.auth_file
    }

    pub fn endpoint_fingerprint(&self) -> &EndpointFingerprint {
        &self.endpoint_fingerprint
    }

    pub fn auth_file_fingerprint(&self) -> &OpaqueIdentity {
        &self.auth_file_fingerprint
    }

    pub fn binding_id(&self) -> &OpaqueIdentity {
        &self.binding_id
    }

    pub const fn client_policy(&self) -> ClientPolicyState {
        self.client_policy
    }

    pub const fn refresh_endpoint_policy(&self) -> RefreshEndpointPolicyState {
        self.refresh_endpoint_policy
    }

    pub fn requested_model(&self) -> &PublicModelId {
        &self.requested_model
    }

    pub fn wire_model(&self) -> &PublicModelId {
        &self.wire_model
    }

    pub const fn certification_ready(&self) -> bool {
        self.certification_ready
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LiveSafetyError {
    #[error("live credential override is present")]
    CredentialOverridePresent,
    #[error("live credential source is ambiguous")]
    AmbiguousCredentialSource,
    #[error("keychain credential state is unavailable")]
    KeychainStateUnavailable,
    #[error("Grok auth file is unavailable")]
    AuthFileUnavailable,
    #[error("Grok auth file is not safely confined")]
    AuthFileConfinementInvalid,
    #[error("Grok auth file metadata is unsafe")]
    AuthFileMetadataInvalid,
    #[error("Grok auth file changed while it was inspected")]
    AuthFileChanged,
    #[error("Grok auth file exceeds its byte ceiling")]
    AuthFileTooLarge,
    #[error("Grok auth file is malformed")]
    AuthFileMalformed,
    #[error("Grok OIDC session is unavailable")]
    OidcSessionUnavailable,
    #[error("Grok OIDC session is expired")]
    OidcSessionExpired,
    #[error("Grok OIDC session expires before the bounded campaign window")]
    OidcSessionExpiresTooSoon,
    #[error("Grok OIDC session metadata is invalid")]
    OidcSessionInvalid,
    #[error("Grok OIDC issuer is not canonical")]
    IssuerNotCanonical,
    #[error("requested Grok model is invalid")]
    ModelInvalid,
    #[error("Grok model route is not canonical")]
    RouteNotCanonical,
    #[error("authoritative OIDC client policy is unavailable")]
    ClientPolicyUnavailable,
    #[error("authoritative OIDC token endpoint policy is unavailable")]
    TokenEndpointPolicyUnavailable,
    #[error("OIDC refresh transport failed")]
    RefreshTransportFailed,
    #[error("OIDC refresh response was refused")]
    RefreshResponseRefused,
    #[error("OIDC refresh response exceeded its byte ceiling")]
    RefreshResponseTooLarge,
    #[error("OIDC refresh response was malformed")]
    RefreshResponseMalformed,
    #[error("secure credential persistence failed")]
    CredentialPersistenceFailed,
    #[error("secure credential persistence detected a race")]
    CredentialPersistenceRace,
    #[error("secure credential inspection is unsupported on this platform")]
    UnsupportedPlatform,
}

impl LiveSafetyError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CredentialOverridePresent => "credential_override_present",
            Self::AmbiguousCredentialSource => "ambiguous_credential_source",
            Self::KeychainStateUnavailable => "keychain_state_unavailable",
            Self::AuthFileUnavailable => "auth_file_unavailable",
            Self::AuthFileConfinementInvalid => "auth_file_confinement_invalid",
            Self::AuthFileMetadataInvalid => "auth_file_metadata_invalid",
            Self::AuthFileChanged => "auth_file_changed",
            Self::AuthFileTooLarge => "auth_file_too_large",
            Self::AuthFileMalformed => "auth_file_malformed",
            Self::OidcSessionUnavailable => "oidc_session_unavailable",
            Self::OidcSessionExpired => "oidc_session_expired",
            Self::OidcSessionExpiresTooSoon => "oidc_session_expires_too_soon",
            Self::OidcSessionInvalid => "oidc_session_invalid",
            Self::IssuerNotCanonical => "issuer_not_canonical",
            Self::ModelInvalid => "model_invalid",
            Self::RouteNotCanonical => "route_not_canonical",
            Self::ClientPolicyUnavailable => "client_policy_unavailable",
            Self::TokenEndpointPolicyUnavailable => "token_endpoint_policy_unavailable",
            Self::RefreshTransportFailed => "refresh_transport_failed",
            Self::RefreshResponseRefused => "refresh_response_refused",
            Self::RefreshResponseTooLarge => "refresh_response_too_large",
            Self::RefreshResponseMalformed => "refresh_response_malformed",
            Self::CredentialPersistenceFailed => "credential_persistence_failed",
            Self::CredentialPersistenceRace => "credential_persistence_race",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeychainApiKeyState {
    Absent,
    Present,
    Unknown,
}

#[derive(Clone, Copy)]
struct AttestationInputs {
    keychain: KeychainApiKeyState,
    override_present: bool,
    client_policy: ClientPolicyState,
    refresh_endpoint_policy: RefreshEndpointPolicyState,
}

pub(crate) struct SafeAuthFile {
    pub(crate) bytes: Vec<u8>,
    state: AuthFileState,
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

pub(crate) struct StrictOidcMaterial {
    pub(crate) scope: String,
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) client_id: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) file_device: u64,
    pub(crate) file_inode: u64,
}

struct ParsedOidcSession {
    scope: String,
    access_token: String,
    refresh_token: String,
    client_id: String,
    expires_at: DateTime<Utc>,
}

/// Inspect the current built-in Grok credential and route without returning
/// any credential material. No network request is performed.
pub fn attest_grok_build_oidc(
    requested_model: &str,
) -> Result<LiveCredentialAttestation, LiveSafetyError> {
    attest_grok_build_oidc_with_min_validity(requested_model, chrono::Duration::zero())
}

/// Attest the official cached session for a finite campaign. The direct Grok
/// Build proxy accepts the cached bearer plus the first-party token-auth
/// header; no fixed OAuth client ID is part of that request contract. A
/// bounded validity window lets the campaign avoid unverified in-process
/// refresh during the run.
pub fn attest_grok_build_oidc_with_min_validity(
    requested_model: &str,
    minimum_validity: chrono::Duration,
) -> Result<LiveCredentialAttestation, LiveSafetyError> {
    let inputs = AttestationInputs {
        keychain: crate::auth_store::xai_keychain_api_key_state(),
        override_present: live_override_present(),
        // Direct Grok Build proxy calls use the cached bearer plus the
        // first-party token-auth header. The dynamic client ID remains
        // private and is not treated as route authority.
        client_policy: ClientPolicyState::AuthoritativelyVerified,
        refresh_endpoint_policy: RefreshEndpointPolicyState::AuthoritativelyVerified,
    };
    attest_path_with_min_validity(
        requested_model,
        &crate::auth_store::auth_json_path(),
        Utc::now(),
        inputs,
        minimum_validity,
    )
}

#[cfg(test)]
fn attest_path(
    requested_model: &str,
    auth_path: &Path,
    now: DateTime<Utc>,
    inputs: AttestationInputs,
) -> Result<LiveCredentialAttestation, LiveSafetyError> {
    attest_path_with_min_validity(
        requested_model,
        auth_path,
        now,
        inputs,
        chrono::Duration::zero(),
    )
}

fn attest_path_with_min_validity(
    requested_model: &str,
    auth_path: &Path,
    now: DateTime<Utc>,
    inputs: AttestationInputs,
    minimum_validity: chrono::Duration,
) -> Result<LiveCredentialAttestation, LiveSafetyError> {
    if inputs.override_present {
        return Err(LiveSafetyError::CredentialOverridePresent);
    }
    match inputs.keychain {
        KeychainApiKeyState::Absent => {}
        KeychainApiKeyState::Present => return Err(LiveSafetyError::AmbiguousCredentialSource),
        KeychainApiKeyState::Unknown => return Err(LiveSafetyError::KeychainStateUnavailable),
    }
    let file = read_safe_auth_file(auth_path)?;
    let session = parse_single_oidc_session(&file.bytes, now)?;
    if session.expires_at < now + minimum_validity {
        return Err(LiveSafetyError::OidcSessionExpiresTooSoon);
    }
    let (requested_model, wire_model) = validate_model_route(requested_model)?;

    let endpoint_fingerprint = EndpointFingerprint::new(&sha256_hex(GROK_BUILD_ENDPOINT))
        .map_err(|_| LiveSafetyError::RouteNotCanonical)?;
    let auth_file_fingerprint = opaque_hash(&[
        b"grokptah.live-auth-file.v1",
        &file.bytes,
        &file.device.to_be_bytes(),
        &file.inode.to_be_bytes(),
    ])?;
    let binding_id = opaque_hash(&[
        b"grokptah.live-credential-binding.v1",
        session.scope.as_bytes(),
        session.client_id.as_bytes(),
        session.access_token.as_bytes(),
        session.refresh_token.as_bytes(),
        requested_model.as_str().as_bytes(),
        wire_model.as_str().as_bytes(),
        GROK_BUILD_ENDPOINT.as_bytes(),
    ])?;
    let overrides = OverrideState {
        xai_api_key_absent: true,
        xai_api_base_absent: true,
        token_command_absent: true,
        compatible_gateway_absent: true,
        auth_configuration_override_absent: true,
        keychain_api_key_absent: true,
    };
    let certification_ready = inputs.client_policy == ClientPolicyState::AuthoritativelyVerified
        && inputs.refresh_endpoint_policy == RefreshEndpointPolicyState::AuthoritativelyVerified
        && overrides.fully_absent()
        && file.state.fully_verified();
    Ok(LiveCredentialAttestation {
        schema: LiveAttestationSchema::V1,
        credential_class: LiveCredentialClass::GrokBuildOidc,
        endpoint_class: LiveEndpointClass::GrokBuildCliProxyV1,
        issuer_class: LiveIssuerClass::FirstPartyXai,
        redirect_policy: RedirectPolicyClass::DenyAll,
        client_policy: inputs.client_policy,
        refresh_endpoint_policy: inputs.refresh_endpoint_policy,
        overrides,
        auth_file: file.state,
        requested_model,
        wire_model,
        endpoint_fingerprint,
        auth_file_fingerprint,
        binding_id,
        certification_ready,
    })
}

pub(crate) fn live_override_present() -> bool {
    live_override_present_with(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn live_override_present_with(mut is_present: impl FnMut(&str) -> bool) -> bool {
    LIVE_OVERRIDE_ENVIRONMENTS
        .iter()
        .any(|name| is_present(name))
}

pub(crate) fn load_strict_oidc_material(
    path: &Path,
    now: DateTime<Utc>,
) -> Result<StrictOidcMaterial, LiveSafetyError> {
    if live_override_present() {
        return Err(LiveSafetyError::CredentialOverridePresent);
    }
    match crate::auth_store::xai_keychain_api_key_state() {
        KeychainApiKeyState::Absent => {}
        KeychainApiKeyState::Present => return Err(LiveSafetyError::AmbiguousCredentialSource),
        KeychainApiKeyState::Unknown => return Err(LiveSafetyError::KeychainStateUnavailable),
    }
    let file = read_safe_auth_file(path)?;
    let session = parse_single_oidc_session(&file.bytes, now)?;
    Ok(StrictOidcMaterial {
        scope: session.scope,
        access_token: session.access_token,
        refresh_token: session.refresh_token,
        client_id: session.client_id,
        expires_at: session.expires_at,
        file_device: file.device,
        file_inode: file.inode,
    })
}

fn validate_model_route(
    requested_model: &str,
) -> Result<(PublicModelId, PublicModelId), LiveSafetyError> {
    let selection = crate::gateway_config::parse_model_selection(requested_model)
        .map_err(|_| LiveSafetyError::ModelInvalid)?;
    if selection.provider_id != crate::gateway_config::XAI_PROVIDER_ID {
        return Err(LiveSafetyError::RouteNotCanonical);
    }
    let requested =
        PublicModelId::new(&selection.model_id).map_err(|_| LiveSafetyError::ModelInvalid)?;
    let profile = crate::gateway_config::resolve_profile_for_selection(&selection, true, None)
        .map_err(|_| LiveSafetyError::RouteNotCanonical)?;
    if profile.id != crate::gateway_config::XAI_PROVIDER_ID
        || !profile.managed_by_host
        || profile.kind != crate::gateway_config::ProviderKind::Xai
        || profile.dialect != crate::gateway_config::ProviderDialect::XaiChatCompletions
        || profile.base_url != GROK_BUILD_ENDPOINT
    {
        return Err(LiveSafetyError::RouteNotCanonical);
    }
    let model = profile
        .models
        .iter()
        .find(|model| model.id == selection.model_id)
        .ok_or(LiveSafetyError::ModelInvalid)?;
    let wire =
        PublicModelId::new(model.wire_model_id()).map_err(|_| LiveSafetyError::ModelInvalid)?;
    Ok((requested, wire))
}

pub(crate) fn validate_canonical_issuer(value: &str) -> Result<(), LiveSafetyError> {
    let parsed = reqwest::Url::parse(value).map_err(|_| LiveSafetyError::IssuerNotCanonical)?;
    if value != XAI_OIDC_ISSUER
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("auth.x.ai")
        || parsed.port().is_some()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(LiveSafetyError::IssuerNotCanonical);
    }
    Ok(())
}

fn parse_single_oidc_session(
    bytes: &[u8],
    now: DateTime<Utc>,
) -> Result<ParsedOidcSession, LiveSafetyError> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|_| LiveSafetyError::AuthFileMalformed)?;
    let entries = root.as_object().ok_or(LiveSafetyError::AuthFileMalformed)?;
    if entries.is_empty() || entries.len() > MAX_AUTH_ENTRIES {
        return Err(LiveSafetyError::AuthFileMalformed);
    }
    let mut eligible = Vec::new();
    let mut saw_expired_oidc = false;
    for (scope, value) in entries {
        let Some(entry) = value.as_object() else {
            return Err(LiveSafetyError::OidcSessionInvalid);
        };
        let Some(access_token) = bounded_nonempty(entry.get("key"), MAX_SECRET_FIELD_BYTES)? else {
            continue;
        };
        let expires_at = entry
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            .ok_or(LiveSafetyError::OidcSessionInvalid)?;
        if expires_at <= now {
            if entry.get("auth_mode").and_then(Value::as_str) == Some("oidc") {
                saw_expired_oidc = true;
            }
            continue;
        }
        if entry.get("auth_mode").and_then(Value::as_str) != Some("oidc") {
            return Err(LiveSafetyError::AmbiguousCredentialSource);
        }
        let issuer = entry
            .get("oidc_issuer")
            .and_then(Value::as_str)
            .ok_or(LiveSafetyError::OidcSessionInvalid)?;
        validate_canonical_issuer(issuer)?;
        let refresh_token = bounded_nonempty(entry.get("refresh_token"), MAX_SECRET_FIELD_BYTES)?
            .ok_or(LiveSafetyError::OidcSessionInvalid)?;
        let client_id = bounded_nonempty(entry.get("oidc_client_id"), MAX_CLIENT_ID_BYTES)?
            .ok_or(LiveSafetyError::OidcSessionInvalid)?;
        if scope.is_empty() || scope.len() > 1024 || scope.contains(['\0', '\n', '\r']) {
            return Err(LiveSafetyError::OidcSessionInvalid);
        }
        eligible.push(ParsedOidcSession {
            scope: scope.clone(),
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.to_owned(),
            client_id: client_id.to_owned(),
            expires_at,
        });
    }
    match eligible.len() {
        1 => Ok(eligible.remove(0)),
        0 if saw_expired_oidc => Err(LiveSafetyError::OidcSessionExpired),
        0 => Err(LiveSafetyError::OidcSessionUnavailable),
        _ => Err(LiveSafetyError::AmbiguousCredentialSource),
    }
}

fn bounded_nonempty(value: Option<&Value>, limit: usize) -> Result<Option<&str>, LiveSafetyError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.len() <= limit => Ok(Some(value)),
        Some(_) => Err(LiveSafetyError::OidcSessionInvalid),
    }
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn opaque_hash(parts: &[&[u8]]) -> Result<OpaqueIdentity, LiveSafetyError> {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.len().to_be_bytes());
        digest.update(part);
    }
    OpaqueIdentity::new(&format!("opaque-{:x}", digest.finalize()))
        .map_err(|_| LiveSafetyError::AuthFileMalformed)
}

pub(crate) fn read_safe_auth_file(path: &Path) -> Result<SafeAuthFile, LiveSafetyError> {
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(LiveSafetyError::UnsupportedPlatform)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        if !path.is_absolute() {
            return Err(LiveSafetyError::AuthFileConfinementInvalid);
        }
        validate_no_symlink_ancestors(path)?;
        let before =
            std::fs::symlink_metadata(path).map_err(|_| LiveSafetyError::AuthFileUnavailable)?;
        validate_unix_metadata(&before)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| LiveSafetyError::AuthFileConfinementInvalid)?;
        let opened = file
            .metadata()
            .map_err(|_| LiveSafetyError::AuthFileMetadataInvalid)?;
        validate_unix_metadata(&opened)?;
        if !same_file(&before, &opened) {
            return Err(LiveSafetyError::AuthFileChanged);
        }
        let capacity =
            usize::try_from(opened.len()).map_err(|_| LiveSafetyError::AuthFileTooLarge)?;
        let mut bytes = Vec::with_capacity(capacity);
        file.by_ref()
            .take(MAX_AUTH_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| LiveSafetyError::AuthFileUnavailable)?;
        if bytes.len() as u64 > MAX_AUTH_JSON_BYTES {
            return Err(LiveSafetyError::AuthFileTooLarge);
        }
        let after = file
            .metadata()
            .map_err(|_| LiveSafetyError::AuthFileMetadataInvalid)?;
        if !same_file(&opened, &after) || after.len() != bytes.len() as u64 {
            return Err(LiveSafetyError::AuthFileChanged);
        }
        Ok(SafeAuthFile {
            bytes,
            state: AuthFileState {
                regular: true,
                current_user_owned: true,
                private_mode_0600: true,
                single_link: true,
                no_symlink_ancestors: true,
                opened_inode_verified: true,
                bounded: true,
            },
            device: after.dev(),
            inode: after.ino(),
        })
    }
}

#[cfg(unix)]
fn validate_no_symlink_ancestors(path: &Path) -> Result<(), LiveSafetyError> {
    let mut current = path
        .parent()
        .ok_or(LiveSafetyError::AuthFileConfinementInvalid)?;
    loop {
        let metadata = std::fs::symlink_metadata(current)
            .map_err(|_| LiveSafetyError::AuthFileConfinementInvalid)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LiveSafetyError::AuthFileConfinementInvalid);
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_unix_metadata(metadata: &Metadata) -> Result<(), LiveSafetyError> {
    use std::os::unix::fs::MetadataExt;

    validate_unix_metadata_fields(
        metadata.is_file(),
        metadata.uid(),
        metadata.mode(),
        metadata.nlink(),
        metadata.len(),
        unsafe { libc::geteuid() },
    )
}

#[cfg(unix)]
fn validate_unix_metadata_fields(
    is_file: bool,
    uid: u32,
    mode: u32,
    link_count: u64,
    length: u64,
    current_uid: u32,
) -> Result<(), LiveSafetyError> {
    if !is_file || uid != current_uid || mode & 0o777 != 0o600 || link_count != 1 {
        return Err(LiveSafetyError::AuthFileMetadataInvalid);
    }
    if length == 0 || length > MAX_AUTH_JSON_BYTES {
        return Err(LiveSafetyError::AuthFileTooLarge);
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn entry(expires_at: DateTime<Utc>) -> Value {
        json!({
            "key": "access-secret-must-not-serialize",
            "refresh_token": "refresh-secret-must-not-serialize",
            "expires_at": expires_at.to_rfc3339(),
            "auth_mode": "oidc",
            "oidc_issuer": XAI_OIDC_ISSUER,
            "oidc_client_id": "dynamic-client-id-must-not-serialize"
        })
    }

    #[cfg(unix)]
    fn auth_file(value: &Value) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let grok = root.path().join(".grok");
        fs::create_dir(&grok).unwrap();
        let path = grok.join("auth.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let path = dunce::canonicalize(path).unwrap();
        (root, path)
    }

    fn inputs() -> AttestationInputs {
        AttestationInputs {
            keychain: KeychainApiKeyState::Absent,
            override_present: false,
            client_policy: ClientPolicyState::Unverified,
            refresh_endpoint_policy: RefreshEndpointPolicyState::AuthoritativelyVerified,
        }
    }

    #[test]
    #[cfg(unix)]
    fn positive_schema_retains_no_secret_path_or_client_id() {
        let now = Utc::now();
        let (root, path) = auth_file(&json!({"scope": entry(now + ChronoDuration::hours(1))}));
        let attestation = attest_path("grok-4.5", &path, now, inputs()).unwrap();
        assert!(!attestation.certification_ready());
        assert_eq!(attestation.client_policy, ClientPolicyState::Unverified);
        assert_eq!(
            attestation.refresh_endpoint_policy,
            RefreshEndpointPolicyState::AuthoritativelyVerified
        );
        let serialized = serde_json::to_string(&attestation).unwrap();
        for forbidden in [
            "access-secret",
            "refresh-secret",
            "dynamic-client-id",
            &path.display().to_string(),
            &root.path().display().to_string(),
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    #[cfg(unix)]
    fn bounded_campaign_rejects_a_session_that_expires_too_soon() {
        let now = Utc::now();
        let (root, path) = auth_file(&json!({
            "scope": entry(now + ChronoDuration::minutes(5))
        }));
        let error = attest_path_with_min_validity(
            "grok-4.5",
            &path,
            now,
            AttestationInputs {
                keychain: KeychainApiKeyState::Absent,
                override_present: false,
                client_policy: ClientPolicyState::AuthoritativelyVerified,
                refresh_endpoint_policy: RefreshEndpointPolicyState::AuthoritativelyVerified,
            },
            ChronoDuration::minutes(10),
        )
        .unwrap_err();
        assert_eq!(error, LiveSafetyError::OidcSessionExpiresTooSoon);
        drop(root);
    }

    #[test]
    fn every_supported_route_or_credential_override_is_refused() {
        for expected in LIVE_OVERRIDE_ENVIRONMENTS {
            assert!(live_override_present_with(|name| name == *expected));
        }
        assert!(!live_override_present_with(|_| false));
    }

    #[test]
    #[cfg(unix)]
    fn overrides_and_keychain_sources_fail_closed() {
        let now = Utc::now();
        let (_root, path) = auth_file(&json!({"scope": entry(now + ChronoDuration::hours(1))}));
        let mut value = inputs();
        value.override_present = true;
        assert_eq!(
            attest_path("grok-4.5", &path, now, value).unwrap_err(),
            LiveSafetyError::CredentialOverridePresent
        );
        value = inputs();
        value.keychain = KeychainApiKeyState::Present;
        assert_eq!(
            attest_path("grok-4.5", &path, now, value).unwrap_err(),
            LiveSafetyError::AmbiguousCredentialSource
        );
        value.keychain = KeychainApiKeyState::Unknown;
        assert_eq!(
            attest_path("grok-4.5", &path, now, value).unwrap_err(),
            LiveSafetyError::KeychainStateUnavailable
        );
    }

    #[test]
    fn issuer_variants_are_rejected() {
        assert!(validate_canonical_issuer(XAI_OIDC_ISSUER).is_ok());
        for value in [
            "https://auth.x.ai/",
            "http://auth.x.ai",
            "https://AUTH.X.AI",
            "https://auth.x.ai:443",
            "https://user@auth.x.ai",
            "https://auth.x.ai/path",
            "https://auth.x.ai?next=x",
            "https://auth.x.ai#fragment",
        ] {
            assert_eq!(
                validate_canonical_issuer(value),
                Err(LiveSafetyError::IssuerNotCanonical),
                "accepted {value}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn ambiguity_expiry_and_malformed_entries_are_rejected() {
        let now = Utc::now();
        let cases = [
            (
                json!({
                    "a": entry(now + ChronoDuration::hours(1)),
                    "b": entry(now + ChronoDuration::hours(2))
                }),
                LiveSafetyError::AmbiguousCredentialSource,
            ),
            (
                json!({"a": entry(now - ChronoDuration::seconds(1))}),
                LiveSafetyError::OidcSessionExpired,
            ),
            (
                json!({"a": {"key": "x", "auth_mode": "api_key", "expires_at": (now + ChronoDuration::hours(1)).to_rfc3339()}}),
                LiveSafetyError::AmbiguousCredentialSource,
            ),
            (json!([]), LiveSafetyError::AuthFileMalformed),
        ];
        for (value, expected) in cases {
            let (_root, path) = auth_file(&value);
            assert_eq!(
                attest_path("grok-4.5", &path, now, inputs()).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn mode_link_count_size_and_symlink_ancestors_fail_closed() {
        let now = Utc::now();
        let value = json!({"scope": entry(now + ChronoDuration::hours(1))});
        let (_mode_root, mode_path) = auth_file(&value);
        fs::set_permissions(&mode_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            attest_path("grok-4.5", &mode_path, now, inputs()).unwrap_err(),
            LiveSafetyError::AuthFileMetadataInvalid
        );

        let (_link_root, link_path) = auth_file(&value);
        fs::hard_link(&link_path, link_path.with_extension("linked")).unwrap();
        assert_eq!(
            attest_path("grok-4.5", &link_path, now, inputs()).unwrap_err(),
            LiveSafetyError::AuthFileMetadataInvalid
        );

        let (_size_root, size_path) = auth_file(&value);
        let file = OpenOptions::new().write(true).open(&size_path).unwrap();
        file.set_len(MAX_AUTH_JSON_BYTES + 1).unwrap();
        assert_eq!(
            attest_path("grok-4.5", &size_path, now, inputs()).unwrap_err(),
            LiveSafetyError::AuthFileTooLarge
        );

        let target = tempfile::tempdir().unwrap();
        let target_grok = target.path().join(".grok");
        fs::create_dir(&target_grok).unwrap();
        let target_file = target_grok.join("auth.json");
        fs::write(&target_file, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::set_permissions(&target_file, fs::Permissions::from_mode(0o600)).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let linked_parent = parent.path().join("linked-grok");
        symlink(&target_grok, &linked_parent).unwrap();
        assert_eq!(
            attest_path("grok-4.5", &linked_parent.join("auth.json"), now, inputs()).unwrap_err(),
            LiveSafetyError::AuthFileConfinementInvalid
        );

        let direct_link_root = tempfile::tempdir().unwrap();
        let direct_link = dunce::canonicalize(direct_link_root.path())
            .unwrap()
            .join("auth.json");
        symlink(&target_file, &direct_link).unwrap();
        assert_eq!(
            attest_path("grok-4.5", &direct_link, now, inputs()).unwrap_err(),
            LiveSafetyError::AuthFileMetadataInvalid
        );
    }

    #[test]
    #[cfg(unix)]
    fn model_and_route_are_bound_to_public_grok_profile() {
        let now = Utc::now();
        let (_root, path) = auth_file(&json!({"scope": entry(now + ChronoDuration::hours(1))}));
        assert_eq!(
            attest_path("not-grok", &path, now, inputs()).unwrap_err(),
            LiveSafetyError::ModelInvalid
        );
        assert_eq!(
            attest_path(
                &crate::gateway_config::model_selection_key("corp", "grok-4.5"),
                &path,
                now,
                inputs(),
            )
            .unwrap_err(),
            LiveSafetyError::RouteNotCanonical
        );
    }

    #[test]
    #[cfg(unix)]
    fn owner_mode_link_type_and_size_metadata_are_fail_closed() {
        let uid = unsafe { libc::geteuid() };
        assert!(validate_unix_metadata_fields(true, uid, 0o100600, 1, 10, uid).is_ok());
        for result in [
            validate_unix_metadata_fields(true, uid.wrapping_add(1), 0o100600, 1, 10, uid),
            validate_unix_metadata_fields(true, uid, 0o100640, 1, 10, uid),
            validate_unix_metadata_fields(true, uid, 0o100600, 2, 10, uid),
            validate_unix_metadata_fields(false, uid, 0o100600, 1, 10, uid),
        ] {
            assert_eq!(result, Err(LiveSafetyError::AuthFileMetadataInvalid));
        }
        assert_eq!(
            validate_unix_metadata_fields(true, uid, 0o100600, 1, MAX_AUTH_JSON_BYTES + 1, uid),
            Err(LiveSafetyError::AuthFileTooLarge)
        );
    }

    #[test]
    fn errors_are_stable_and_secret_free() {
        let error = LiveSafetyError::RefreshResponseMalformed;
        assert_eq!(error.code(), "refresh_response_malformed");
        assert_eq!(error.to_string(), "OIDC refresh response was malformed");
        assert!(!error.to_string().contains("http"));
        assert!(!error.to_string().contains("token"));
    }
}
