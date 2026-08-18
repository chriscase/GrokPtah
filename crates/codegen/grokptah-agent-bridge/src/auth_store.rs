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

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use keyring::Entry;
use serde_json::Value;

use crate::types::AuthState;

const SERVICE: &str = "grokptah-desktop";
const ACCOUNT_API_KEY: &str = "xai-api-key";
const ACCOUNT_DISPLAY: &str = "display-name";
const PROVIDER_KEYCHAIN_PREFIX: &str = "keychain:";

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

/// Avoid concurrent refresh stampedes (async-friendly).
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn grok_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
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
    load_grok_build_session()
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
    let profile = crate::gateway_config::resolve_profile_for_selection(&selection, false)?;
    resolve_profile_credentials(&profile, Some(&selection.model_id))
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
    let profile = crate::gateway_config::resolve_profile_for_selection(&selection, false)?;
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

/// Refresh access token if missing/near expiry. Best-effort; returns original on failure.
pub async fn ensure_fresh_credentials(creds: WireCredentials) -> WireCredentials {
    if !creds.oidc_token_auth {
        return creds;
    }
    let needs = creds.expires_at.is_none_or(|exp| {
        // Refresh 5 minutes early (same spirit as CLI proactive refresh).
        exp < Utc::now() + ChronoDuration::minutes(5)
    });
    if !needs {
        return creds;
    }
    match refresh_oidc(&creds).await {
        Ok(fresh) => fresh,
        Err(e) => {
            eprintln!("[grokptah] OIDC refresh skipped/failed: {e}");
            creds
        }
    }
}

/// Force a refresh (e.g. after HTTP 401).
pub async fn force_refresh(creds: &WireCredentials) -> Result<WireCredentials, String> {
    refresh_oidc(creds).await
}

async fn refresh_oidc(creds: &WireCredentials) -> Result<WireCredentials, String> {
    let _guard = REFRESH_LOCK.lock().await;

    // Re-read disk — another process may have refreshed already.
    if let Some(disk) = load_grok_build_session() {
        if disk
            .expires_at
            .is_some_and(|exp| exp > Utc::now() + ChronoDuration::minutes(5))
            && disk.bearer != creds.bearer
        {
            return Ok(disk);
        }
        // Prefer latest disk material for refresh fields.
        return refresh_oidc_inner(&disk).await;
    }
    refresh_oidc_inner(creds).await
}

async fn refresh_oidc_inner(creds: &WireCredentials) -> Result<WireCredentials, String> {
    let refresh = creds
        .refresh_token
        .as_deref()
        .ok_or_else(|| "no refresh_token in auth.json — run `grok login`".to_string())?;
    let issuer = creds.oidc_issuer.as_deref().unwrap_or("https://auth.x.ai");
    let client_id = creds
        .oidc_client_id
        .as_deref()
        .ok_or_else(|| "no oidc_client_id — run `grok login`".to_string())?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    // OIDC discovery
    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let disc: Value = client
        .get(&discovery_url)
        .send()
        .await
        .map_err(|e| format!("OIDC discovery: {e}"))?
        .error_for_status()
        .map_err(|e| format!("OIDC discovery status: {e}"))?
        .json()
        .await
        .map_err(|e| format!("OIDC discovery json: {e}"))?;
    let token_endpoint = disc
        .get("token_endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "discovery missing token_endpoint".to_string())?;

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", client_id),
    ];
    if let Some(pt) = creds.principal_type.as_deref() {
        form.push(("principal_type", pt));
    }
    if let Some(pid) = creds.principal_id.as_deref() {
        form.push(("principal_id", pid));
    }

    let resp = client
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("token refresh request: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token refresh HTTP {status}: {body}"));
    }
    let tokens: Value = resp
        .json()
        .await
        .map_err(|e| format!("token refresh json: {e}"))?;
    let access = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "no access_token in refresh response".to_string())?;
    let new_refresh = tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| creds.refresh_token.clone());
    let expires_in = tokens
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    let expires_at = Utc::now() + ChronoDuration::seconds(expires_in as i64);

    // Persist back into ~/.grok/auth.json so CLI + GrokPtah stay in sync.
    if let Some(scope) = &creds.auth_scope {
        if let Err(e) = write_refreshed_auth(scope, access, new_refresh.as_deref(), expires_at) {
            eprintln!("[grokptah] failed to write refreshed auth.json: {e}");
        }
    }

    let mut fresh = creds.clone();
    fresh.bearer = access.to_string();
    fresh.refresh_token = new_refresh;
    fresh.expires_at = Some(expires_at);
    Ok(fresh)
}

fn write_refreshed_auth(
    scope: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<(), String> {
    let path = auth_json_path();
    let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut root: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "auth.json root not object".to_string())?;
    let entry = obj
        .get_mut(scope)
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| format!("scope {scope} missing"))?;
    entry.insert("key".into(), Value::String(access_token.into()));
    if let Some(rt) = refresh_token {
        entry.insert("refresh_token".into(), Value::String(rt.into()));
    }
    entry.insert("expires_at".into(), Value::String(expires_at.to_rfc3339()));
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_auth_state() -> AuthState {
    if let Some(w) = resolve_wire_credentials() {
        return AuthState {
            signed_in: true,
            display_name: Some(w.display_name),
            method: Some(w.method),
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
}
