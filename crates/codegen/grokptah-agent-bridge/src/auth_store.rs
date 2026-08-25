//! Credentials for live model calls.
//!
//! Priority:
//! 1. `XAI_API_KEY` env
//! 2. GrokPtah OS keychain API key
//! 3. **Grok Build session** from `~/.grok/auth.json` (same file as `grok` CLI / browser login)
//!
//! OIDC sessions must hit `cli-chat-proxy` with the same headers as Grok Build:
//! - `Authorization: Bearer <jwt>`
//! - `X-XAI-Token-Auth: xai-grok-cli`  (**not** `"true"`)
//! - `x-authenticateresponse: authenticate-response`
//!
//! We also refresh the access token via the OIDC refresh_token when near expiry
//! or after a 401.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use keyring::Entry;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::grok_account::{project_grok_account_status, GrokAccountFacts, GrokCredentialMethod};
use crate::types::AuthState;

const SERVICE: &str = "grokptah-desktop";
const ACCOUNT_API_KEY: &str = "xai-api-key";
const ACCOUNT_DISPLAY: &str = "display-name";
const PROVIDER_KEYCHAIN_PREFIX: &str = "keychain:";
const OIDC_DISCOVERY_PATH: &str = "/.well-known/openid-configuration";
const REFRESH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REFRESH_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 32 * 1024;
const MAX_REFRESH_EXPIRES_SECONDS: u64 = 24 * 60 * 60;

pub(crate) fn xai_keychain_api_key_state() -> crate::live_attestation::KeychainApiKeyState {
    let Ok(entry) = Entry::new(SERVICE, ACCOUNT_API_KEY) else {
        return crate::live_attestation::KeychainApiKeyState::Unknown;
    };
    match entry.get_password() {
        Ok(value) if value.is_empty() => crate::live_attestation::KeychainApiKeyState::Absent,
        Ok(_) => crate::live_attestation::KeychainApiKeyState::Present,
        Err(keyring::Error::NoEntry) => crate::live_attestation::KeychainApiKeyState::Absent,
        Err(_) => crate::live_attestation::KeychainApiKeyState::Unknown,
    }
}

/// Header value required by cli-chat-proxy nginx auth (matches xai-grok-cli).
pub const XAI_TOKEN_AUTH_VALUE: &str = "xai-grok-cli";
pub const XAI_AUTHENTICATE_RESPONSE: &str = "authenticate-response";
/// Proxy version gate (`HTTP 426` if missing/too old). Must be ≥ 0.1.202.
///
/// **Important:** do not put `grokptah-…` in parentheses. The proxy’s parser
/// treats the parenthetical as the version — `0.2.101 (grokptah-0.1.0)` is
/// read as `0.1.0` and rejected. Use a clean CLI-compatible version only.
pub fn client_version_header() -> String {
    if let Ok(v) = std::env::var("GROK_VERSION") {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    if let Some(v) = detect_installed_grok_version() {
        return v;
    }
    // Known-good floor that passes cli-chat-proxy (matches current stable CLI).
    "0.2.101".to_string()
}

#[cfg(test)]
mod version_header_tests {
    use super::client_version_header;

    #[test]
    fn client_version_has_no_grokptah_parenthetical() {
        let v = client_version_header();
        assert!(
            !v.to_lowercase().contains("grokptah"),
            "proxy mis-parses grokptah-… in parentheses as the version: got {v:?}"
        );
        assert!(
            v.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "version must start with a digit: {v:?}"
        );
    }
}

/// Parse `grok --version` → e.g. `0.2.101 (5bc4b5dfadcf)` when available.
fn detect_installed_grok_version() -> Option<String> {
    let output = std::process::Command::new("grok")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Examples: "grok 0.2.101 (5bc4b5dfadcf) [stable]" or "0.2.101"
    let line = text.lines().next()?.trim();
    let rest = line
        .strip_prefix("grok ")
        .or_else(|| line.strip_prefix("Grok "))
        .unwrap_or(line)
        .trim();
    // Drop channel suffix " [stable]"
    let rest = rest.split(" [").next()?.trim();
    if rest.is_empty() {
        return None;
    }
    // Sanity: must start with a digit
    if !rest.chars().next()?.is_ascii_digit() {
        return None;
    }
    Some(rest.to_string())
}

#[derive(Clone)]
pub struct WireCredentials {
    /// Provider profile that owns this credential. Request routing must match it.
    pub provider_id: String,
    /// Bearer token (OIDC JWT `key` from auth.json, or API key).
    pub bearer: String,
    /// When true, send CLI OIDC headers (not bare API key).
    pub oidc_token_auth: bool,
    pub display_name: String,
    pub method: String,
    pub user_id: Option<String>,
    pub team_id: Option<String>,
    /// Auth.json map key (scope) for writing refreshed tokens back.
    pub auth_scope: Option<String>,
    pub refresh_token: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    pub principal_type: Option<String>,
    pub principal_id: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl WireCredentials {
    /// Stable, non-secret identity used to bind measured provider capabilities
    /// to the credential principal that was actually qualified. OIDC access
    /// token rotation does not change the fingerprint when durable principal
    /// fields are available. API keys and legacy OIDC records fall back to a
    /// one-way digest of high-entropy credential material.
    pub(crate) fn qualification_identity_fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"grokptah.provider-qualification-identity.v1\0");
        update_fingerprint_field(&mut digest, "provider", Some(&self.provider_id));
        update_fingerprint_field(
            &mut digest,
            "mode",
            Some(if self.oidc_token_auth {
                "oidc"
            } else {
                "api_key"
            }),
        );
        if self.oidc_token_auth {
            let has_stable_principal = self.principal_id.is_some()
                || self.user_id.is_some()
                || self.team_id.is_some()
                || self.auth_scope.is_some();
            if has_stable_principal {
                update_fingerprint_field(&mut digest, "issuer", self.oidc_issuer.as_deref());
                update_fingerprint_field(&mut digest, "client", self.oidc_client_id.as_deref());
                update_fingerprint_field(
                    &mut digest,
                    "principal_type",
                    self.principal_type.as_deref(),
                );
                update_fingerprint_field(&mut digest, "principal_id", self.principal_id.as_deref());
                update_fingerprint_field(&mut digest, "user", self.user_id.as_deref());
                update_fingerprint_field(&mut digest, "team", self.team_id.as_deref());
                update_fingerprint_field(&mut digest, "scope", self.auth_scope.as_deref());
            } else {
                update_fingerprint_field(
                    &mut digest,
                    "legacy_secret",
                    self.refresh_token.as_deref().or(Some(&self.bearer)),
                );
            }
        } else {
            update_fingerprint_field(&mut digest, "api_key", Some(&self.bearer));
        }
        format!("v1-sha256:{:x}", digest.finalize())
    }

    /// Classify this credential into the closed editor vocabulary.
    ///
    /// Derived from structured fields, never from [`Self::method`] for the xAI
    /// profile: that string is built from `auth_mode` inside `auth.json`, so it
    /// is file-controlled and must not select an authority class. Compatible
    /// profiles use the two method strings this crate writes itself; anything
    /// else fails closed to [`GrokCredentialMethod::Unknown`].
    pub(crate) fn account_method(&self) -> GrokCredentialMethod {
        if self.provider_id == crate::gateway_config::XAI_PROVIDER_ID {
            if self.oidc_token_auth {
                GrokCredentialMethod::GrokBuildOidc
            } else {
                GrokCredentialMethod::XaiApiKey
            }
        } else {
            match self.method.as_str() {
                "provider_env" => GrokCredentialMethod::GatewayManaged,
                "provider_keychain" => GrokCredentialMethod::GatewayApiKey,
                _ => GrokCredentialMethod::Unknown,
            }
        }
    }

    /// Non-secret account facts for the public status projection.
    ///
    /// Copies only account identifiers and the expiry. `bearer` and
    /// `refresh_token` have no field to land in on [`GrokAccountFacts`].
    pub(crate) fn account_facts(&self) -> GrokAccountFacts {
        GrokAccountFacts {
            provider_id: self.provider_id.clone(),
            method: self.account_method(),
            display_name: Some(self.display_name.clone()),
            oidc_issuer: self.oidc_issuer.clone(),
            oidc_client_id: self.oidc_client_id.clone(),
            principal_type: self.principal_type.clone(),
            principal_id: self.principal_id.clone(),
            user_id: self.user_id.clone(),
            team_id: self.team_id.clone(),
            expires_at: self.expires_at,
        }
    }
}

fn update_fingerprint_field(digest: &mut Sha256, label: &str, value: Option<&str>) {
    digest.update(label.len().to_be_bytes());
    digest.update(label.as_bytes());
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.len().to_be_bytes());
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
}

/// Avoid concurrent refresh stampedes (async-friendly).
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub const GROK_HOME_ENV: &str = "GROK_HOME";

/// Match the official Grok CLI's cache-location override. Keeping this in one
/// resolver means direct proxy calls, refreshes, and strict certification
/// attestation cannot silently consult different auth stores.
pub fn grok_home() -> PathBuf {
    std::env::var_os(GROK_HOME_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".grok")
        })
}

pub fn auth_json_path() -> PathBuf {
    grok_home().join("auth.json")
}

/// Best credential for the built-in xAI profile.
///
/// Compatible providers use [`resolve_wire_credentials_for_model`] so an xAI
/// credential can never be combined with a corporate endpoint.
pub fn resolve_wire_credentials() -> Option<WireCredentials> {
    resolve_xai_credentials()
}

fn resolve_xai_credentials() -> Option<WireCredentials> {
    resolve_xai_api_key_credentials().or_else(load_grok_build_session)
}

fn resolve_xai_api_key_credentials() -> Option<WireCredentials> {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if !key.is_empty() {
            return Some(WireCredentials {
                provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
                bearer: key,
                oidc_token_auth: false,
                display_name: "env:XAI_API_KEY".into(),
                method: "api_key".into(),
                user_id: None,
                team_id: None,
                auth_scope: None,
                refresh_token: None,
                oidc_issuer: None,
                oidc_client_id: None,
                principal_type: None,
                principal_id: None,
                expires_at: None,
            });
        }
    }
    if let Ok(entry) = Entry::new(SERVICE, ACCOUNT_API_KEY) {
        if let Ok(key) = entry.get_password() {
            if !key.is_empty() {
                let name = Entry::new(SERVICE, ACCOUNT_DISPLAY)
                    .ok()
                    .and_then(|e| e.get_password().ok())
                    .unwrap_or_else(|| "API key".into());
                return Some(WireCredentials {
                    provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
                    bearer: key,
                    oidc_token_auth: false,
                    display_name: name,
                    method: "api_key".into(),
                    user_id: None,
                    team_id: None,
                    auth_scope: None,
                    refresh_token: None,
                    oidc_issuer: None,
                    oidc_client_id: None,
                    principal_type: None,
                    principal_id: None,
                    expires_at: None,
                });
            }
        }
    }
    // The rotating xAI token helper is intentionally part of the xAI profile.
    // Compatible-provider token helpers must be explicit profile references;
    // otherwise a command's token could be attached to the wrong endpoint.
    if let Ok(cmd) = std::env::var("GROKPTAH_TOKEN_COMMAND") {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            if let Some(tok) = run_token_command(cmd) {
                return Some(WireCredentials {
                    provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
                    bearer: tok,
                    oidc_token_auth: false,
                    display_name: "token_command".into(),
                    method: "token_command".into(),
                    user_id: None,
                    team_id: None,
                    auth_scope: None,
                    refresh_token: None,
                    oidc_issuer: None,
                    oidc_client_id: None,
                    principal_type: None,
                    principal_id: None,
                    expires_at: None,
                });
            }
        }
    }
    None
}

/// Opaque keychain reference stored in a provider profile.
pub fn provider_keychain_ref(profile_id: &str) -> Result<String, String> {
    let profile_id = crate::gateway_config::normalized_profile_id(profile_id)?;
    Ok(format!(
        "{PROVIDER_KEYCHAIN_PREFIX}provider/{profile_id}/api-key"
    ))
}

fn keychain_account_from_ref(reference: &str) -> Result<&str, String> {
    let account = reference
        .strip_prefix(PROVIDER_KEYCHAIN_PREFIX)
        .ok_or_else(|| "unsupported provider credential reference".to_string())?;
    if account.is_empty() || account.len() > 128 || account.contains(['\0', '\n', '\r']) {
        return Err("invalid provider credential reference".into());
    }
    Ok(account)
}

/// Store and read back a provider secret before returning its durable reference.
pub fn store_provider_api_key(profile_id: &str, api_key: &str) -> Result<String, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("provider API key is empty".into());
    }
    let reference = provider_keychain_ref(profile_id)?;
    let account = keychain_account_from_ref(&reference)?;
    let entry = Entry::new(SERVICE, account).map_err(|error| error.to_string())?;
    entry
        .set_password(api_key)
        .map_err(|error| format!("store provider credential: {error}"))?;
    let verified = entry
        .get_password()
        .map_err(|error| format!("verify provider credential: {error}"))?;
    if verified != api_key {
        return Err("provider credential verification failed".into());
    }
    Ok(reference)
}

fn read_provider_credential(reference: &str) -> Result<String, String> {
    if let Some(variable) = reference.strip_prefix("env:") {
        if variable.is_empty()
            || !variable
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err("invalid provider environment credential reference".into());
        }
        return std::env::var(variable)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!("provider credential environment variable {variable} is unset")
            });
    }
    let account = keychain_account_from_ref(reference)?;
    Entry::new(SERVICE, account)
        .map_err(|error| error.to_string())?
        .get_password()
        .map_err(|error| format!("read provider credential: {error}"))
        .and_then(|value| {
            if value.trim().is_empty() {
                Err("provider credential is empty".into())
            } else {
                Ok(value)
            }
        })
}

fn validate_provider_credential_ref(
    profile_id: &str,
    managed_by_env: bool,
    reference: &str,
) -> Result<(), String> {
    if reference.starts_with("env:") {
        let expected = match profile_id {
            "env-grokptah" if managed_by_env => "env:GROKPTAH_API_KEY",
            "env-openai" if managed_by_env => "env:OPENAI_API_KEY",
            _ => {
                return Err("environment credential reference is not owned by this profile".into())
            }
        };
        if reference != expected {
            return Err("environment credential reference does not match its profile".into());
        }
        return Ok(());
    }
    if reference != provider_keychain_ref(profile_id)? {
        return Err("keychain credential reference does not match its provider profile".into());
    }
    Ok(())
}

pub fn provider_credential_is_set(profile_id: &str, managed_by_env: bool, reference: &str) -> bool {
    validate_provider_credential_ref(profile_id, managed_by_env, reference).is_ok()
        && read_provider_credential(reference).is_ok()
}

pub fn delete_provider_credential(profile_id: &str, reference: &str) -> Result<(), String> {
    validate_provider_credential_ref(profile_id, false, reference)?;
    if reference.starts_with("env:") {
        return Ok(());
    }
    let account = keychain_account_from_ref(reference)?;
    let entry = Entry::new(SERVICE, account).map_err(|error| error.to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("delete provider credential: {error}")),
    }
}

fn compatible_credentials(profile_id: &str, reference: &str, bearer: String) -> WireCredentials {
    WireCredentials {
        provider_id: profile_id.to_string(),
        bearer,
        oidc_token_auth: false,
        display_name: profile_id.to_string(),
        method: if reference.starts_with("env:") {
            "provider_env"
        } else {
            "provider_keychain"
        }
        .into(),
        user_id: None,
        team_id: None,
        auth_scope: None,
        refresh_token: None,
        oidc_issuer: None,
        oidc_client_id: None,
        principal_type: None,
        principal_id: None,
        expires_at: None,
    }
}

fn migrate_legacy_provider_credential<F>(
    config: &mut crate::gateway_config::GatewayConfig,
    profile: &crate::gateway_config::ProviderProfile,
    store_verified: F,
) -> Result<Option<WireCredentials>, String>
where
    F: FnOnce(&str, &str) -> Result<String, String>,
{
    if !config.has_pending_legacy_secret()
        || config.active_profile_id.as_deref() != Some(profile.id.as_str())
    {
        return Ok(None);
    }

    let legacy_secret = config.api_key.clone();
    let reference = store_verified(&profile.id, &legacy_secret)?;
    let mut migrated = config.clone();
    migrated
        .profile_mut(&profile.id)
        .ok_or_else(|| "provider profile disappeared during migration".to_string())?
        .credential_ref = Some(reference.clone());
    migrated.clear_legacy_fields();
    crate::gateway_config::save(&migrated)
        .map_err(|error| format!("finalize provider credential migration: {error}"))?;
    *config = migrated;
    Ok(Some(compatible_credentials(
        &profile.id,
        &reference,
        legacy_secret,
    )))
}

/// Resolve the credential owned by the model's exact provider profile.
///
/// A pending v1 plaintext key is migrated only on use: the keychain write is
/// read back first, then config is atomically rewritten without the plaintext.
/// Any failure leaves the original file untouched and returns an error.
pub fn resolve_wire_credentials_for_model(
    model_selection: &str,
) -> Result<Option<WireCredentials>, String> {
    let selection = crate::gateway_config::parse_model_selection(model_selection)?;
    let profile = crate::gateway_config::resolve_profile_for_selection(&selection, false, None)?;
    resolve_profile_credentials(&profile, Some(&selection.model_id))
}

/// Resolve only the credential reference frozen onto a provider route.
///
/// This deliberately does not load the mutable provider catalog. A finite Run
/// may refresh the same durable credential principal, but changing a profile's
/// credential reference while the Run is active cannot reroute its authority.
pub(crate) fn resolve_wire_credentials_for_route(
    provider_id: &str,
    kind: crate::gateway_config::ProviderKind,
    dialect: crate::gateway_config::ProviderDialect,
    credential_ref: &str,
) -> Result<Option<WireCredentials>, String> {
    use crate::gateway_config::{ProviderDialect, ProviderKind, XAI_PROVIDER_ID};

    match (kind, dialect) {
        (ProviderKind::Xai, ProviderDialect::XaiChatCompletions)
            if provider_id == XAI_PROVIDER_ID =>
        {
            match credential_ref {
                "managed:xai:api-key" => Ok(resolve_xai_api_key_credentials()),
                "managed:xai:oidc" => {
                    Ok(load_grok_build_session().filter(|credentials| credentials.oidc_token_auth))
                }
                _ => Err("xAI provider route has an unsupported credential reference".into()),
            }
        }
        (ProviderKind::OpenAiCompatible, ProviderDialect::OpenAiChatCompletions)
            if provider_id != XAI_PROVIDER_ID =>
        {
            let managed_by_env = matches!(provider_id, "env-grokptah" | "env-openai");
            validate_provider_credential_ref(provider_id, managed_by_env, credential_ref)?;
            let bearer = read_provider_credential(credential_ref)?;
            Ok(Some(compatible_credentials(
                provider_id,
                credential_ref,
                bearer,
            )))
        }
        _ => Err("provider route has an unsupported credential boundary".into()),
    }
}

fn resolve_profile_credentials(
    profile: &crate::gateway_config::ProviderProfile,
    legacy_model_id: Option<&str>,
) -> Result<Option<WireCredentials>, String> {
    use crate::gateway_config::{ProviderDialect, ProviderKind, XAI_PROVIDER_ID};

    match (profile.kind, profile.dialect) {
        (ProviderKind::Xai, ProviderDialect::XaiChatCompletions)
            if profile.id == XAI_PROVIDER_ID && profile.managed_by_host =>
        {
            Ok(resolve_xai_credentials())
        }
        (ProviderKind::OpenAiCompatible, ProviderDialect::OpenAiChatCompletions)
            if !profile.managed_by_host =>
        {
            resolve_stored_provider_credentials(&profile.id, legacy_model_id)
        }
        _ => Err(
            "xAI credentials are available only to the synthesized host-managed `xai` profile"
                .into(),
        ),
    }
}

pub fn resolve_provider_credentials(
    provider_id: &str,
    legacy_model_id: Option<&str>,
) -> Result<Option<WireCredentials>, String> {
    let provider_id = if provider_id.trim() == crate::gateway_config::XAI_PROVIDER_ID {
        crate::gateway_config::XAI_PROVIDER_ID.to_string()
    } else {
        crate::gateway_config::normalized_profile_id(provider_id)?
    };
    let selection = crate::gateway_config::ModelSelection {
        provider_id,
        model_id: legacy_model_id.unwrap_or_default().to_string(),
    };
    let profile = crate::gateway_config::resolve_profile_for_selection(&selection, false, None)?;
    resolve_profile_credentials(&profile, legacy_model_id)
}

fn resolve_stored_provider_credentials(
    provider_id: &str,
    legacy_model_id: Option<&str>,
) -> Result<Option<WireCredentials>, String> {
    let mut config = crate::gateway_config::load_for_update()
        .map_err(|error| format!("read provider profiles: {error}"))?;
    if config.has_pending_legacy_secret() {
        let profile = config
            .profile_mut(provider_id)
            .ok_or_else(|| format!("unknown provider profile `{provider_id}`"))?;
        if let Some(model_id) = legacy_model_id
            .filter(|model_id| !profile.models.iter().any(|model| model.id == **model_id))
        {
            let mut legacy_model = crate::gateway_config::ProviderModel::unqualified(model_id);
            // Preserve the only behavior the v1 gateway exposed. This is
            // migration evidence, not a claim about newly added models.
            legacy_model.capabilities.tools = true;
            legacy_model.capabilities.stream = true;
            legacy_model.capabilities.parallel_tool_calls = true;
            legacy_model.capabilities.source = crate::gateway_config::CapabilitySource::Declared;
            profile.upsert_model(legacy_model);
        }
    }
    let profile = config
        .profile(provider_id)
        .cloned()
        .ok_or_else(|| format!("unknown provider profile `{provider_id}`"))?;

    if let Some(credentials) =
        migrate_legacy_provider_credential(&mut config, &profile, store_provider_api_key)?
    {
        return Ok(Some(credentials));
    }

    let Some(reference) = profile.credential_ref.as_deref() else {
        return Ok(None);
    };
    validate_provider_credential_ref(&profile.id, profile.managed_by_env, reference)?;
    let bearer = read_provider_credential(reference)?;
    Ok(Some(compatible_credentials(&profile.id, reference, bearer)))
}

fn run_token_command(cmd: &str) -> Option<String> {
    // Shell out once; never log stdout (may be a secret).
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    let tok = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    if tok.is_empty() {
        None
    } else {
        Some(tok.to_string())
    }
}

/// Read the active OIDC session from Grok Build's `~/.grok/auth.json`.
fn load_grok_build_session() -> Option<WireCredentials> {
    let path = auth_json_path();
    let raw = fs::read_to_string(&path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    let obj = root.as_object()?;

    let mut best_expired: Option<WireCredentials> = None;
    for (scope, entry) in obj {
        let Some(cred) = entry.as_object() else {
            continue;
        };
        let Some(key) = cred
            .get("key")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let email = cred
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("Grok Build session");
        let first = cred
            .get("first_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let display = if !first.is_empty() {
            format!("{first} ({email})")
        } else {
            email.to_string()
        };
        let mode = cred
            .get("auth_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("oidc");
        // User sessions always need the CLI token-auth header on cli-chat-proxy.
        let oidc =
            mode == "oidc" || mode.contains("oidc") || mode == "user" || mode == "user_token";
        let expires_at = cred
            .get("expires_at")
            .and_then(|v| v.as_str())
            .and_then(|exp| DateTime::parse_from_rfc3339(exp).ok())
            .map(|t| t.with_timezone(&Utc));
        let expired = expires_at.is_some_and(|t| t < Utc::now());
        let candidate = WireCredentials {
            provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
            bearer: key.to_string(),
            oidc_token_auth: oidc || mode != "api_key",
            display_name: display,
            method: format!("grok_build:{mode}"),
            user_id: cred
                .get("user_id")
                .or_else(|| cred.get("principal_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            team_id: cred
                .get("team_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            auth_scope: Some(scope.clone()),
            refresh_token: cred
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            oidc_issuer: cred
                .get("oidc_issuer")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            oidc_client_id: cred
                .get("oidc_client_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            principal_type: cred
                .get("principal_type")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            principal_id: cred
                .get("principal_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            expires_at,
        };
        if !expired {
            return Some(candidate);
        }
        best_expired = Some(candidate);
    }
    best_expired
}

/// Apply Authorization + OIDC CLI headers expected by cli-chat-proxy.
pub fn apply_auth_headers(
    mut req: reqwest::RequestBuilder,
    creds: &WireCredentials,
    base_url: &str,
) -> reqwest::RequestBuilder {
    req = req.header("Authorization", format!("Bearer {}", creds.bearer));
    // Version gate applies to all cli-chat-proxy traffic (OIDC and otherwise).
    let is_proxy = base_url.contains("cli-chat-proxy") || creds.oidc_token_auth;
    if is_proxy {
        // Missing this header → HTTP 426 "CLI version (none) is outdated".
        req = req.header("x-grok-client-version", client_version_header());
        // Same metric label family as the interactive CLI (not headless `-p`).
        req = req.header("x-grok-client-mode", "interactive");
    }
    if is_proxy && creds.oidc_token_auth {
        // MUST be the CLI product id — `"true"` is rejected as unknown.
        req = req
            .header("X-XAI-Token-Auth", XAI_TOKEN_AUTH_VALUE)
            .header("x-authenticateresponse", XAI_AUTHENTICATE_RESPONSE);
        if let Some(uid) = &creds.user_id {
            req = req.header("x-userid", uid);
        }
        if let Some(tid) = &creds.team_id {
            req = req.header("x-teamid", tid);
        }
    }
    req
}

/// Refresh an OIDC access token near expiry. A refusal is secret-free and
/// leaves the original credential unchanged; callers may surface only its code.
pub async fn ensure_fresh_credentials(creds: WireCredentials) -> WireCredentials {
    if !creds.oidc_token_auth {
        return creds;
    }
    let needs = creds
        .expires_at
        .is_none_or(|exp| exp < Utc::now() + ChronoDuration::minutes(5));
    if !needs {
        return creds;
    }
    match refresh_oidc(&creds).await {
        Ok(fresh) => fresh,
        Err(error) => {
            eprintln!("[grokptah] OIDC refresh refused: {}", error.code());
            creds
        }
    }
}

/// Force a bounded first-party OIDC refresh, for example after HTTP 401.
pub async fn force_refresh(
    creds: &WireCredentials,
) -> Result<WireCredentials, crate::live_attestation::LiveSafetyError> {
    refresh_oidc(creds).await
}

async fn refresh_oidc(
    creds: &WireCredentials,
) -> Result<WireCredentials, crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::LiveSafetyError;

    if !creds.oidc_token_auth {
        return Err(LiveSafetyError::OidcSessionInvalid);
    }
    let _guard = REFRESH_LOCK.lock().await;
    let path = auth_json_path();
    let material = crate::live_attestation::load_strict_oidc_material(&path, Utc::now())?;
    if creds.auth_scope.as_deref() != Some(material.scope.as_str())
        || creds.oidc_client_id.as_deref() != Some(material.client_id.as_str())
    {
        return Err(LiveSafetyError::AmbiguousCredentialSource);
    }
    let mut disk = creds.clone();
    disk.bearer = material.access_token;
    disk.refresh_token = Some(material.refresh_token);
    disk.oidc_issuer = Some(crate::live_attestation::XAI_OIDC_ISSUER.into());
    disk.oidc_client_id = Some(material.client_id);
    disk.auth_scope = Some(material.scope);
    disk.expires_at = Some(material.expires_at);
    if disk
        .expires_at
        .is_some_and(|expiry| expiry > Utc::now() + ChronoDuration::minutes(5))
        && disk.bearer != creds.bearer
    {
        return Ok(disk);
    }
    refresh_oidc_inner(&disk, material.file_device, material.file_inode).await
}

async fn refresh_oidc_inner(
    creds: &WireCredentials,
    expected_device: u64,
    expected_inode: u64,
) -> Result<WireCredentials, crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::{LiveSafetyError, XAI_OIDC_ISSUER, XAI_OIDC_TOKEN_ENDPOINT};

    let refresh = creds
        .refresh_token
        .as_deref()
        .filter(|value| !value.is_empty() && value.len() <= MAX_REFRESH_TOKEN_BYTES)
        .ok_or(LiveSafetyError::OidcSessionInvalid)?;
    let issuer = creds
        .oidc_issuer
        .as_deref()
        .ok_or(LiveSafetyError::OidcSessionInvalid)?;
    crate::live_attestation::validate_canonical_issuer(issuer)?;
    let client_id = creds
        .oidc_client_id
        .as_deref()
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .ok_or(LiveSafetyError::ClientPolicyUnavailable)?;
    let client = reqwest::Client::builder()
        .connect_timeout(REFRESH_CONNECT_TIMEOUT)
        .timeout(REFRESH_OPERATION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| LiveSafetyError::RefreshTransportFailed)?;

    let discovery_url = format!("{XAI_OIDC_ISSUER}{OIDC_DISCOVERY_PATH}");
    let discovery_response = client
        .get(discovery_url)
        .send()
        .await
        .map_err(|_| LiveSafetyError::RefreshTransportFailed)?;
    let discovery = bounded_refresh_json(discovery_response).await?;
    validate_discovery_document(&discovery)?;

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", client_id),
    ];
    if let Some(principal_type) = creds.principal_type.as_deref() {
        if principal_type.len() > 256 {
            return Err(LiveSafetyError::OidcSessionInvalid);
        }
        form.push(("principal_type", principal_type));
    }
    if let Some(principal_id) = creds.principal_id.as_deref() {
        if principal_id.len() > 1024 {
            return Err(LiveSafetyError::OidcSessionInvalid);
        }
        form.push(("principal_id", principal_id));
    }
    let token_response = client
        .post(XAI_OIDC_TOKEN_ENDPOINT)
        .form(&form)
        .send()
        .await
        .map_err(|_| LiveSafetyError::RefreshTransportFailed)?;
    let tokens = bounded_refresh_json(token_response).await?;
    let access = bounded_refresh_string(&tokens, "access_token")?;
    let new_refresh = match tokens.get("refresh_token") {
        None => refresh.to_owned(),
        Some(Value::String(value))
            if !value.is_empty() && value.len() <= MAX_REFRESH_TOKEN_BYTES =>
        {
            value.clone()
        }
        Some(_) => return Err(LiveSafetyError::RefreshResponseMalformed),
    };
    let expires_in = tokens
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|value| (1..=MAX_REFRESH_EXPIRES_SECONDS).contains(value))
        .ok_or(LiveSafetyError::RefreshResponseMalformed)?;
    let expires_at = Utc::now()
        + ChronoDuration::seconds(
            i64::try_from(expires_in).map_err(|_| LiveSafetyError::RefreshResponseMalformed)?,
        );
    let scope = creds
        .auth_scope
        .as_deref()
        .ok_or(LiveSafetyError::OidcSessionInvalid)?;
    write_refreshed_auth_at(
        &auth_json_path(),
        expected_device,
        expected_inode,
        scope,
        &creds.bearer,
        access,
        Some(&new_refresh),
        expires_at,
    )?;

    let mut fresh = creds.clone();
    fresh.bearer = access.to_owned();
    fresh.refresh_token = Some(new_refresh);
    fresh.expires_at = Some(expires_at);
    Ok(fresh)
}

fn validate_discovery_document(
    discovery: &Value,
) -> Result<(), crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::LiveSafetyError;

    let issuer = discovery
        .get("issuer")
        .and_then(Value::as_str)
        .ok_or(LiveSafetyError::RefreshResponseMalformed)?;
    crate::live_attestation::validate_canonical_issuer(issuer)?;
    let token_endpoint = discovery
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or(LiveSafetyError::RefreshResponseMalformed)?;
    validate_token_endpoint(token_endpoint)
}

fn validate_token_endpoint(value: &str) -> Result<(), crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::{LiveSafetyError, XAI_OIDC_TOKEN_ENDPOINT};

    let parsed =
        reqwest::Url::parse(value).map_err(|_| LiveSafetyError::TokenEndpointPolicyUnavailable)?;
    if value != XAI_OIDC_TOKEN_ENDPOINT
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("auth.x.ai")
        || parsed.port().is_some()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/oauth2/token"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(LiveSafetyError::TokenEndpointPolicyUnavailable);
    }
    Ok(())
}

async fn bounded_refresh_json(
    mut response: reqwest::Response,
) -> Result<Value, crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::LiveSafetyError;

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    validate_refresh_response_metadata(response.status(), content_type, response.content_length())?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LiveSafetyError::RefreshTransportFailed)?
    {
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > MAX_REFRESH_RESPONSE_BYTES)
        {
            return Err(LiveSafetyError::RefreshResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    parse_refresh_json(&body)
}

fn validate_refresh_response_metadata(
    status: reqwest::StatusCode,
    content_type: &str,
    content_length: Option<u64>,
) -> Result<(), crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::LiveSafetyError;

    if status.is_redirection() || !status.is_success() {
        return Err(LiveSafetyError::RefreshResponseRefused);
    }
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(LiveSafetyError::RefreshResponseMalformed);
    }
    if content_length.is_some_and(|length| length > MAX_REFRESH_RESPONSE_BYTES as u64) {
        return Err(LiveSafetyError::RefreshResponseTooLarge);
    }
    Ok(())
}

fn parse_refresh_json(bytes: &[u8]) -> Result<Value, crate::live_attestation::LiveSafetyError> {
    use crate::live_attestation::LiveSafetyError;

    if bytes.is_empty() || bytes.len() > MAX_REFRESH_RESPONSE_BYTES {
        return Err(if bytes.len() > MAX_REFRESH_RESPONSE_BYTES {
            LiveSafetyError::RefreshResponseTooLarge
        } else {
            LiveSafetyError::RefreshResponseMalformed
        });
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| LiveSafetyError::RefreshResponseMalformed)?;
    if !value.is_object() {
        return Err(LiveSafetyError::RefreshResponseMalformed);
    }
    Ok(value)
}

fn bounded_refresh_string<'a>(
    value: &'a Value,
    key: &str,
) -> Result<&'a str, crate::live_attestation::LiveSafetyError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_REFRESH_TOKEN_BYTES)
        .ok_or(crate::live_attestation::LiveSafetyError::RefreshResponseMalformed)
}

#[allow(clippy::too_many_arguments)]
fn write_refreshed_auth_at(
    path: &Path,
    expected_device: u64,
    expected_inode: u64,
    scope: &str,
    expected_access_token: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<(), crate::live_attestation::LiveSafetyError> {
    #[cfg(not(unix))]
    {
        let _ = (
            path,
            expected_device,
            expected_inode,
            scope,
            expected_access_token,
            access_token,
            refresh_token,
            expires_at,
        );
        Err(crate::live_attestation::LiveSafetyError::UnsupportedPlatform)
    }
    #[cfg(unix)]
    {
        use crate::live_attestation::LiveSafetyError;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        if access_token.is_empty()
            || access_token.len() > MAX_REFRESH_TOKEN_BYTES
            || refresh_token
                .is_some_and(|value| value.is_empty() || value.len() > MAX_REFRESH_TOKEN_BYTES)
        {
            return Err(LiveSafetyError::RefreshResponseMalformed);
        }
        let snapshot = crate::live_attestation::read_safe_auth_file(path)?;
        if snapshot.device != expected_device || snapshot.inode != expected_inode {
            return Err(LiveSafetyError::CredentialPersistenceRace);
        }
        let mut root: Value = serde_json::from_slice(&snapshot.bytes)
            .map_err(|_| LiveSafetyError::AuthFileMalformed)?;
        let entry = root
            .as_object_mut()
            .and_then(|root| root.get_mut(scope))
            .and_then(Value::as_object_mut)
            .ok_or(LiveSafetyError::CredentialPersistenceRace)?;
        if entry.get("key").and_then(Value::as_str) != Some(expected_access_token) {
            return Err(LiveSafetyError::CredentialPersistenceRace);
        }
        entry.insert("key".into(), Value::String(access_token.into()));
        if let Some(refresh_token) = refresh_token {
            entry.insert("refresh_token".into(), Value::String(refresh_token.into()));
        }
        entry.insert("expires_at".into(), Value::String(expires_at.to_rfc3339()));
        let bytes = serde_json::to_vec_pretty(&root)
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        if bytes.is_empty() || bytes.len() as u64 > crate::live_attestation::MAX_AUTH_JSON_BYTES {
            return Err(LiveSafetyError::CredentialPersistenceFailed);
        }
        let parent = path
            .parent()
            .ok_or(LiveSafetyError::CredentialPersistenceFailed)?;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(LiveSafetyError::CredentialPersistenceFailed)?;
        let temp_path = parent.join(format!(
            ".{file_name}.grokptah-{}.tmp",
            Uuid::new_v4().simple()
        ));
        let mut cleanup = ExactTempCleanup::new(temp_path.clone());
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temp_path)
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        temp.write_all(&bytes)
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        temp.flush()
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        temp.sync_all()
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        let metadata = temp
            .metadata()
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() != bytes.len() as u64
        {
            return Err(LiveSafetyError::CredentialPersistenceFailed);
        }
        drop(temp);
        let current = crate::live_attestation::read_safe_auth_file(path)?;
        if current.device != expected_device
            || current.inode != expected_inode
            || current.bytes != snapshot.bytes
        {
            return Err(LiveSafetyError::CredentialPersistenceRace);
        }
        fs::rename(&temp_path, path).map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        cleanup.disarm();
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| LiveSafetyError::CredentialPersistenceFailed)?;
        let installed = crate::live_attestation::read_safe_auth_file(path)?;
        if installed.bytes != bytes {
            return Err(LiveSafetyError::CredentialPersistenceFailed);
        }
        Ok(())
    }
}

struct ExactTempCleanup {
    path: PathBuf,
    armed: bool,
}

impl ExactTempCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ExactTempCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn load_auth_state() -> AuthState {
    if let Some(w) = resolve_wire_credentials() {
        // `signed_in` keeps its existing meaning — a credential resolved — so
        // older desktop clients are unaffected. `account` is the accurate view:
        // a resolved-but-expired Grok Build session reports `session: expired`
        // and `usable: false` there, which is what a run gate must read.
        let account = project_grok_account_status(&w.account_facts(), Utc::now());
        return AuthState {
            signed_in: true,
            display_name: Some(w.display_name),
            method: Some(w.method),
            account: Some(account),
        };
    }
    AuthState::default()
}

pub fn store_api_key(api_key: &str, display_name: &str) -> Result<AuthState, String> {
    let entry = Entry::new(SERVICE, ACCOUNT_API_KEY).map_err(|e| e.to_string())?;
    entry.set_password(api_key).map_err(|e| e.to_string())?;
    if let Ok(e) = Entry::new(SERVICE, ACCOUNT_DISPLAY) {
        let _ = e.set_password(display_name);
    }
    Ok(AuthState {
        signed_in: true,
        display_name: Some(display_name.into()),
        method: Some("api_key".into()),
        account: Some(project_grok_account_status(
            &GrokAccountFacts {
                provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
                method: GrokCredentialMethod::XaiApiKey,
                display_name: Some(display_name.into()),
                ..GrokAccountFacts::default()
            },
            Utc::now(),
        )),
    })
}

pub fn clear_credentials() -> AuthState {
    if let Ok(e) = Entry::new(SERVICE, ACCOUNT_API_KEY) {
        let _ = e.delete_credential();
    }
    if let Ok(e) = Entry::new(SERVICE, ACCOUNT_DISPLAY) {
        let _ = e.delete_credential();
    }
    // Do not delete ~/.grok/auth.json — that is shared with the official CLI.
    load_auth_state()
}

#[allow(dead_code)]
pub fn get_api_key() -> Option<String> {
    resolve_wire_credentials().map(|w| w.bearer)
}

/// Open browser to xAI console for API keys / account.
pub fn open_login_page() -> Result<String, String> {
    let url = "https://console.x.ai/";
    open::that(url).map_err(|e| e.to_string())?;
    Ok(url.into())
}

/// Tell the user how to get a Grok Build session if none is present.
pub fn auth_help_message() -> String {
    let path = auth_json_path();
    format!(
        "No live credentials. Either:\n\
         • Run `grok login` (or `cargo run -p xai-grok-pager-bin` and sign in) so `{}` exists, or\n\
         • Paste an xAI API key from https://console.x.ai (Save key), or\n\
         • export XAI_API_KEY=...",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};
    use crate::gateway_config::{
        model_selection_key, GatewayConfig, ProviderModel, ProviderProfile,
    };

    fn expect_credential_error(result: Result<Option<WireCredentials>, String>) -> String {
        match result {
            Ok(_) => panic!("credential operation unexpectedly succeeded"),
            Err(error) => error,
        }
    }

    fn fingerprint_credentials(
        bearer: &str,
        oidc: bool,
        user_id: Option<&str>,
        team_id: Option<&str>,
    ) -> WireCredentials {
        WireCredentials {
            provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
            bearer: bearer.into(),
            oidc_token_auth: oidc,
            display_name: "test".into(),
            method: if oidc { "oidc" } else { "api_key" }.into(),
            user_id: user_id.map(str::to_string),
            team_id: team_id.map(str::to_string),
            auth_scope: Some("test-scope".into()),
            refresh_token: Some("refresh-secret".into()),
            oidc_issuer: Some("https://auth.x.ai".into()),
            oidc_client_id: Some("test-client".into()),
            principal_type: Some("user".into()),
            principal_id: user_id.map(str::to_string),
            expires_at: None,
        }
    }

    #[test]
    fn qualification_fingerprint_tracks_principal_without_persisting_secrets() {
        let api_a = fingerprint_credentials("api-secret-a", false, None, None);
        let api_b = fingerprint_credentials("api-secret-b", false, None, None);
        let api_a_fingerprint = api_a.qualification_identity_fingerprint();
        assert_ne!(
            api_a_fingerprint,
            api_b.qualification_identity_fingerprint()
        );
        assert!(!api_a_fingerprint.contains("api-secret-a"));

        let oidc_before =
            fingerprint_credentials("access-before", true, Some("user-a"), Some("team-a"));
        let oidc_after =
            fingerprint_credentials("access-after", true, Some("user-a"), Some("team-a"));
        let other_principal =
            fingerprint_credentials("access-after", true, Some("user-b"), Some("team-a"));
        assert_eq!(
            oidc_before.qualification_identity_fingerprint(),
            oidc_after.qualification_identity_fingerprint(),
            "access-token rotation for one durable principal must keep its qualification"
        );
        assert_ne!(
            oidc_before.qualification_identity_fingerprint(),
            other_principal.qualification_identity_fingerprint()
        );
    }

    #[test]
    fn credentials_are_bound_to_the_selected_provider_profile() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));
        unsafe {
            std::env::set_var("GROKPTAH_API_BASE", "https://a.example/v1");
            std::env::set_var("GROKPTAH_API_KEY", "synthetic-a");
            std::env::set_var("OPENAI_BASE_URL", "https://b.example/v1");
            std::env::set_var("OPENAI_API_KEY", "synthetic-b");
            std::env::set_var("XAI_API_KEY", "synthetic-xai");
        }

        let a =
            resolve_wire_credentials_for_model(&model_selection_key("env-grokptah", "code-model"))
                .unwrap()
                .unwrap();
        let b =
            resolve_wire_credentials_for_model(&model_selection_key("env-openai", "code-model"))
                .unwrap()
                .unwrap();
        let xai = resolve_wire_credentials_for_model("grok-4.5")
            .unwrap()
            .unwrap();
        let xai_by_profile = resolve_provider_credentials("xai", Some("grok-4.5"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (a.provider_id.as_str(), a.bearer.as_str()),
            ("env-grokptah", "synthetic-a")
        );
        assert_eq!(
            (b.provider_id.as_str(), b.bearer.as_str()),
            ("env-openai", "synthetic-b")
        );
        assert_eq!(
            (xai.provider_id.as_str(), xai.bearer.as_str()),
            ("xai", "synthetic-xai")
        );
        assert_eq!(xai_by_profile.provider_id, xai.provider_id);
        assert_eq!(xai_by_profile.bearer, xai.bearer);
        assert_eq!(xai_by_profile.oidc_token_auth, xai.oidc_token_auth);
        assert_eq!(xai_by_profile.method, xai.method);
        assert_eq!(xai_by_profile.display_name, xai.display_name);

        let mismatch = crate::host_helpers::resolve_model_target(
            &a,
            &model_selection_key("env-openai", "code-model"),
        )
        .unwrap_err();
        assert!(mismatch
            .to_string()
            .contains("provider credential mismatch"));

        unsafe {
            std::env::remove_var("GROKPTAH_API_BASE");
            std::env::remove_var("GROKPTAH_API_KEY");
            std::env::remove_var("OPENAI_BASE_URL");
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("XAI_API_KEY");
        }
        set_grokptah_home_override(None);
    }

    #[test]
    fn frozen_route_credentials_resolve_without_the_mutable_provider_catalog() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));
        unsafe {
            std::env::set_var("GROKPTAH_API_KEY", "synthetic-frozen-route-key");
            std::env::set_var("OPENAI_API_KEY", "synthetic-other-route-key");
        }

        // No gateway profile is needed: the Run already froze the trusted
        // provider identity and credential reference at admission.
        let resolved = resolve_wire_credentials_for_route(
            "env-grokptah",
            crate::gateway_config::ProviderKind::OpenAiCompatible,
            crate::gateway_config::ProviderDialect::OpenAiChatCompletions,
            "env:GROKPTAH_API_KEY",
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.provider_id, "env-grokptah");
        assert_eq!(resolved.bearer, "synthetic-frozen-route-key");

        let error = expect_credential_error(resolve_wire_credentials_for_route(
            "env-grokptah",
            crate::gateway_config::ProviderKind::OpenAiCompatible,
            crate::gateway_config::ProviderDialect::OpenAiChatCompletions,
            "env:OPENAI_API_KEY",
        ));
        assert!(error.contains("does not match its profile"));

        unsafe {
            std::env::remove_var("GROKPTAH_API_KEY");
            std::env::remove_var("OPENAI_API_KEY");
        }
        set_grokptah_home_override(None);
    }

    #[test]
    fn crafted_cross_profile_references_fail_closed() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));

        let mut config = GatewayConfig::default();
        let mut profile = ProviderProfile::openai_compatible("corp-a", "A", "https://a.example/v1");
        profile.credential_ref = Some("keychain:provider/corp-b/api-key".into());
        profile.upsert_model(ProviderModel::unqualified("model"));
        config.upsert_profile(profile).unwrap();
        crate::gateway_config::save(&config).unwrap();
        let error = expect_credential_error(resolve_provider_credentials("corp-a", Some("model")));
        assert!(error.contains("does not match"));

        let mut config = crate::gateway_config::load_for_update().unwrap();
        config.profile_mut("corp-a").unwrap().credential_ref = Some("env:XAI_API_KEY".into());
        crate::gateway_config::save(&config).unwrap();
        let error = expect_credential_error(resolve_provider_credentials("corp-a", Some("model")));
        assert!(error.contains("not owned"));

        set_grokptah_home_override(None);
    }

    #[test]
    fn legacy_migration_retains_the_only_secret_on_failure_and_is_idempotent() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        let config_path = home.join("gateway.json");
        fs::write(
            &config_path,
            r#"{"provider_id":"legacy-corp","base_url":"https://legacy.example/v1","api_key":"synthetic-legacy-secret"}"#,
        )
        .unwrap();
        set_grokptah_home_override(Some(home));

        let mut config = crate::gateway_config::load_for_update().unwrap();
        let profile = config.profile("legacy-corp").unwrap().clone();
        let error = expect_credential_error(migrate_legacy_provider_credential(
            &mut config,
            &profile,
            |_, _| Err("protected store unavailable".into()),
        ));
        assert!(error.contains("protected store unavailable"));
        assert!(config.has_pending_legacy_secret());
        assert!(fs::read_to_string(&config_path)
            .unwrap()
            .contains("synthetic-legacy-secret"));

        let migrated = migrate_legacy_provider_credential(&mut config, &profile, |id, secret| {
            assert_eq!(id, "legacy-corp");
            assert_eq!(secret, "synthetic-legacy-secret");
            Ok("keychain:provider/legacy-corp/api-key".into())
        })
        .unwrap()
        .unwrap();
        assert_eq!(migrated.provider_id, "legacy-corp");
        assert_eq!(migrated.bearer, "synthetic-legacy-secret");
        let raw = fs::read_to_string(&config_path).unwrap();
        assert!(!raw.contains("synthetic-legacy-secret"));
        assert!(!raw.contains("api_key"));
        assert!(raw.contains("keychain:provider/legacy-corp/api-key"));

        assert!(
            migrate_legacy_provider_credential(&mut config, &profile, |_, _| {
                panic!("idempotent migration must not write the protected store twice")
            })
            .unwrap()
            .is_none()
        );

        set_grokptah_home_override(None);
    }

    #[test]
    fn first_party_refresh_endpoint_policy_is_exact() {
        assert!(validate_token_endpoint(crate::live_attestation::XAI_OIDC_TOKEN_ENDPOINT).is_ok());
        for endpoint in [
            "http://auth.x.ai/oauth2/token",
            "https://AUTH.X.AI/oauth2/token",
            "https://auth.x.ai/oauth2/token/",
            "https://auth.x.ai:443/oauth2/token",
            "https://auth.x.ai/other",
            "https://user@auth.x.ai/oauth2/token",
            "https://auth.x.ai/oauth2/token?next=x",
            "https://auth.x.ai/oauth2/token#fragment",
            "https://example.invalid/oauth2/token",
        ] {
            assert_eq!(
                validate_token_endpoint(endpoint),
                Err(crate::live_attestation::LiveSafetyError::TokenEndpointPolicyUnavailable),
                "accepted {endpoint}"
            );
        }
    }

    #[test]
    fn discovery_document_binds_issuer_and_token_endpoint() {
        use crate::live_attestation::{LiveSafetyError, XAI_OIDC_TOKEN_ENDPOINT};

        assert!(validate_discovery_document(&serde_json::json!({
            "issuer": crate::live_attestation::XAI_OIDC_ISSUER,
            "token_endpoint": XAI_OIDC_TOKEN_ENDPOINT
        }))
        .is_ok());
        assert_eq!(
            validate_discovery_document(&serde_json::json!({
                "issuer": "https://auth.x.ai/",
                "token_endpoint": XAI_OIDC_TOKEN_ENDPOINT
            })),
            Err(LiveSafetyError::IssuerNotCanonical)
        );
        assert_eq!(
            validate_discovery_document(&serde_json::json!({
                "issuer": crate::live_attestation::XAI_OIDC_ISSUER,
                "token_endpoint": "https://auth.x.ai/oauth2/token/"
            })),
            Err(LiveSafetyError::TokenEndpointPolicyUnavailable)
        );
        assert_eq!(
            validate_discovery_document(&serde_json::json!({})),
            Err(LiveSafetyError::RefreshResponseMalformed)
        );
    }

    #[test]
    fn refresh_metadata_and_json_are_bounded_and_secret_free() {
        use crate::live_attestation::LiveSafetyError;

        assert_eq!(
            validate_refresh_response_metadata(
                reqwest::StatusCode::FOUND,
                "application/json",
                Some(2),
            ),
            Err(LiveSafetyError::RefreshResponseRefused)
        );
        assert_eq!(
            validate_refresh_response_metadata(reqwest::StatusCode::OK, "text/html", Some(2),),
            Err(LiveSafetyError::RefreshResponseMalformed)
        );
        assert_eq!(
            validate_refresh_response_metadata(
                reqwest::StatusCode::OK,
                "application/json",
                Some(MAX_REFRESH_RESPONSE_BYTES as u64 + 1),
            ),
            Err(LiveSafetyError::RefreshResponseTooLarge)
        );
        assert_eq!(
            parse_refresh_json(b"not-json"),
            Err(LiveSafetyError::RefreshResponseMalformed)
        );
        assert_eq!(
            parse_refresh_json(&vec![b'x'; MAX_REFRESH_RESPONSE_BYTES + 1]),
            Err(LiveSafetyError::RefreshResponseTooLarge)
        );
        let rendered = LiveSafetyError::RefreshResponseRefused.to_string();
        assert_eq!(rendered, "OIDC refresh response was refused");
        assert!(!rendered.contains("http"));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn refresh_transport_limits_are_finite_and_ordered() {
        assert_eq!(REFRESH_CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(REFRESH_OPERATION_TIMEOUT, Duration::from_secs(20));
        assert!(REFRESH_CONNECT_TIMEOUT < REFRESH_OPERATION_TIMEOUT);
        assert_eq!(MAX_REFRESH_RESPONSE_BYTES, 64 * 1024);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_refresh_write_preserves_private_file_and_cleans_temp() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let expires_at = Utc::now() + ChronoDuration::hours(1);
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "scope": {
                    "key": "old-access",
                    "refresh_token": "old-refresh",
                    "expires_at": expires_at.to_rfc3339(),
                    "auth_mode": "oidc",
                    "oidc_issuer": crate::live_attestation::XAI_OIDC_ISSUER,
                    "oidc_client_id": "dynamic-client"
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let path = dunce::canonicalize(path).unwrap();
        let snapshot = crate::live_attestation::read_safe_auth_file(&path).unwrap();
        write_refreshed_auth_at(
            &path,
            snapshot.device,
            snapshot.inode,
            "scope",
            "old-access",
            "new-access",
            Some("new-refresh"),
            expires_at + ChronoDuration::hours(1),
        )
        .unwrap();
        let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(installed["scope"]["key"], "new-access");
        assert_eq!(installed["scope"]["refresh_token"], "new-refresh");
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".grokptah-")
        }));
    }

    #[test]
    #[cfg(unix)]
    fn atomic_refresh_write_rejects_replaced_original() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let expires_at = Utc::now() + ChronoDuration::hours(1);
        let document = |key: &str| {
            serde_json::to_vec(&serde_json::json!({
                "scope": {
                    "key": key,
                    "refresh_token": "refresh",
                    "expires_at": expires_at.to_rfc3339(),
                    "auth_mode": "oidc",
                    "oidc_issuer": crate::live_attestation::XAI_OIDC_ISSUER,
                    "oidc_client_id": "dynamic-client"
                }
            }))
            .unwrap()
        };
        fs::write(&path, document("old-access")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let path = dunce::canonicalize(path).unwrap();
        let snapshot = crate::live_attestation::read_safe_auth_file(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&path, document("replacement-access")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            write_refreshed_auth_at(
                &path,
                snapshot.device,
                snapshot.inode,
                "scope",
                "old-access",
                "new-access",
                Some("new-refresh"),
                expires_at,
            ),
            Err(crate::live_attestation::LiveSafetyError::CredentialPersistenceRace)
        );
        let retained: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(retained["scope"]["key"], "replacement-access");
    }
}
