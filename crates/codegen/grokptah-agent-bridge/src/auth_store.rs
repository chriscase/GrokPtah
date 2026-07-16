//! Credentials for live model calls.
//!
//! Priority:
//! 1. `XAI_API_KEY` env
//! 2. GrokPtah OS keychain API key
//! 3. **Grok Build session** from `~/.grok/auth.json` (same file as `grok` CLI / browser login)

use std::fs;
use std::path::PathBuf;

use keyring::Entry;
use serde_json::Value;

use crate::types::AuthState;

const SERVICE: &str = "grokptah-desktop";
const ACCOUNT_API_KEY: &str = "xai-api-key";
const ACCOUNT_DISPLAY: &str = "display-name";

#[derive(Debug, Clone)]
pub struct WireCredentials {
    /// Bearer token (OIDC JWT `key` from auth.json, or API key).
    pub bearer: String,
    /// When true, also send `X-XAI-Token-Auth: true` (OIDC user sessions).
    pub oidc_token_auth: bool,
    pub display_name: String,
    pub method: String,
}

pub fn grok_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
}

pub fn auth_json_path() -> PathBuf {
    grok_home().join("auth.json")
}

/// Best credential for outbound xAI chat API calls.
pub fn resolve_wire_credentials() -> Option<WireCredentials> {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if !key.is_empty() {
            return Some(WireCredentials {
                bearer: key,
                oidc_token_auth: false,
                display_name: "env:XAI_API_KEY".into(),
                method: "api_key".into(),
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
                });
            }
        }
    }
    load_grok_build_session()
}

/// Read the active OIDC session from Grok Build's `~/.grok/auth.json`.
fn load_grok_build_session() -> Option<WireCredentials> {
    let path = auth_json_path();
    let raw = fs::read_to_string(&path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    let obj = root.as_object()?;

    // File is a map of scope keys → credential objects (or nested shapes).
    let mut best: Option<(bool, WireCredentials)> = None; // (expired, creds)
    for (_scope, entry) in obj {
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
        let oidc = mode == "oidc" || mode.contains("oidc");
        let expired = cred
            .get("expires_at")
            .and_then(|v| v.as_str())
            .and_then(|exp| chrono::DateTime::parse_from_rfc3339(exp).ok())
            .is_some_and(|t| t < chrono::Utc::now());
        let candidate = WireCredentials {
            bearer: key.to_string(),
            oidc_token_auth: oidc,
            display_name: display,
            method: format!("grok_build:{mode}"),
        };
        if !expired {
            return Some(candidate);
        }
        if best.as_ref().is_none_or(|(was_exp, _)| *was_exp) {
            best = Some((true, candidate));
        }
    }
    best.map(|(_, c)| c)
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
