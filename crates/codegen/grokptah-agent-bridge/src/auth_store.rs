//! Secure credential storage for desktop auth (OS keyring + env fallback).

use keyring::Entry;

use crate::types::AuthState;

const SERVICE: &str = "grokptah-desktop";
const ACCOUNT_API_KEY: &str = "xai-api-key";
const ACCOUNT_DISPLAY: &str = "display-name";

pub fn load_auth_state() -> AuthState {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if !key.is_empty() {
            return AuthState {
                signed_in: true,
                display_name: Some("env:XAI_API_KEY".into()),
                method: Some("api_key".into()),
            };
        }
    }
    if let Ok(entry) = Entry::new(SERVICE, ACCOUNT_API_KEY) {
        if let Ok(key) = entry.get_password() {
            if !key.is_empty() {
                let name = Entry::new(SERVICE, ACCOUNT_DISPLAY)
                    .ok()
                    .and_then(|e| e.get_password().ok())
                    .unwrap_or_else(|| "API key".into());
                return AuthState {
                    signed_in: true,
                    display_name: Some(name),
                    method: Some("api_key".into()),
                };
            }
        }
    }
    AuthState::default()
}

pub fn store_api_key(api_key: &str, display_name: &str) -> Result<AuthState, String> {
    let entry = Entry::new(SERVICE, ACCOUNT_API_KEY).map_err(|e| e.to_string())?;
    entry
        .set_password(api_key)
        .map_err(|e| e.to_string())?;
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
    AuthState::default()
}

pub fn get_api_key() -> Option<String> {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    Entry::new(SERVICE, ACCOUNT_API_KEY)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|k| !k.is_empty())
}

/// Open browser to xAI console / CLI auth documentation for obtaining a key.
pub fn open_login_page() -> Result<String, String> {
    let url = "https://console.x.ai/";
    open::that(url).map_err(|e| e.to_string())?;
    Ok(url.into())
}
