//! Corporate / OpenAI-compatible gateway config (#169).
//!
//! Stored under `~/.grokptah/gateway.json`. Env vars still win when set so
//! operators can override without the UI. Default xAI OIDC / `XAI_API_KEY`
//! path is unchanged when this file is absent.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::discover::grokptah_home;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayConfig {
    /// Optional provider label (Build-style `model_providers.<id>` name).
    #[serde(default)]
    pub provider_id: String,
    /// OpenAI-compatible base URL, e.g. `https://gateway.example/v1`.
    #[serde(default)]
    pub base_url: String,
    /// Bearer token for the gateway (not the xAI OIDC session).
    #[serde(default)]
    pub api_key: String,
}

fn path() -> PathBuf {
    grokptah_home().join("gateway.json")
}

pub fn load() -> GatewayConfig {
    let p = path();
    let Ok(raw) = fs::read_to_string(&p) else {
        return GatewayConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save(cfg: &GatewayConfig) -> std::io::Result<()> {
    let p = path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".into());
    fs::write(p, raw)
}

/// Effective base URL: env overrides file.
pub fn effective_base_url() -> Option<String> {
    for key in [
        "XAI_API_BASE",
        "GROKPTAH_API_BASE",
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
    ] {
        if let Ok(v) = std::env::var(key) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    let cfg = load();
    let t = cfg.base_url.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Gateway API key from file only when env keys are unset (xAI env still preferred by auth_store).
pub fn file_api_key() -> Option<String> {
    let cfg = load();
    let t = cfg.api_key.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};

    #[test]
    fn roundtrip_gateway_json() {
        let _lock = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));
        let cfg = GatewayConfig {
            provider_id: "corp".into(),
            base_url: "https://gw.example/v1".into(),
            api_key: "sk-test".into(),
        };
        save(&cfg).unwrap();
        let loaded = load();
        assert_eq!(loaded.base_url, "https://gw.example/v1");
        assert_eq!(loaded.api_key, "sk-test");
        assert_eq!(loaded.provider_id, "corp");
        set_grokptah_home_override(None);
    }

    #[test]
    fn effective_base_prefers_env_over_file() {
        let _lock = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));
        save(&GatewayConfig {
            provider_id: "corp".into(),
            base_url: "https://from-file/v1".into(),
            api_key: String::new(),
        })
        .unwrap();
        // SAFETY: test-only env mutation under serial lock
        unsafe {
            std::env::set_var("GROKPTAH_API_BASE", "https://from-env/v1");
        }
        let base = effective_base_url().unwrap();
        assert_eq!(base, "https://from-env/v1");
        unsafe {
            std::env::remove_var("GROKPTAH_API_BASE");
        }
        assert_eq!(
            effective_base_url().as_deref(),
            Some("https://from-file/v1")
        );
        set_grokptah_home_override(None);
    }
}
