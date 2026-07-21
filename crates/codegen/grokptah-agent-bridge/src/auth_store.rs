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

#[derive(Debug, Clone)]
pub struct WireCredentials {
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

/// Best credential for outbound chat API calls.
///
/// Order (must not break default xAI path — #169):
/// 1. `XAI_API_KEY`
/// 2. Keyring API key
/// 3. Corporate/OpenAI-compatible: `GROKPTAH_API_KEY` / `OPENAI_API_KEY`
/// 4. Rotating token command (#170): `GROKPTAH_TOKEN_COMMAND`
/// 5. Grok Build OIDC session (`~/.grok/auth.json`)
pub fn resolve_wire_credentials() -> Option<WireCredentials> {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if !key.is_empty() {
            return Some(WireCredentials {
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
    // Corporate / OpenAI-compatible gateway keys (#169) — only when xAI key absent.
    for (env_name, label) in [
        ("GROKPTAH_API_KEY", "env:GROKPTAH_API_KEY"),
        ("OPENAI_API_KEY", "env:OPENAI_API_KEY"),
    ] {
        if let Ok(key) = std::env::var(env_name) {
            if !key.is_empty() {
                return Some(WireCredentials {
                    bearer: key,
                    oidc_token_auth: false,
                    display_name: label.into(),
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
    // Settings UI gateway.json key (#169) when env keys absent.
    if let Some(key) = crate::gateway_config::file_api_key() {
        return Some(WireCredentials {
            bearer: key,
            oidc_token_auth: false,
            display_name: "gateway.json".into(),
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
    // Rotating token helper (#170): command prints a short-lived bearer to stdout.
    if let Ok(cmd) = std::env::var("GROKPTAH_TOKEN_COMMAND") {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            if let Some(tok) = run_token_command(cmd) {
                return Some(WireCredentials {
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
