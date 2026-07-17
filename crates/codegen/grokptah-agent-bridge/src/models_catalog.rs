//! Model catalog shared with Grok Build.
//!
//! Grok Build writes the live server list to `~/.grok/models_cache.json` and the
//! user default to `~/.grok/config.toml` (`[models] default`). GrokPtah used to
//! hardcode a short outdated list (`grok-2`/`grok-3`/…) and default to `grok-3`.
//! Prefer the same cache + config so the desktop dropdown matches the TUI.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::auth_store::grok_home;
use crate::types::ModelInfo;

/// Fallback when cache is missing / unreadable (still prefer Grok Build ids).
const BUILTIN: &[(&str, &str, bool)] = &[
    ("grok-build", "Grok Build", true),
    ("grok-4.5", "Grok 4.5", true),
    ("grok-4", "Grok 4", true),
    ("grok-3", "Grok 3", true),
    ("grok-3-mini", "Grok 3 Mini", false),
    ("grok-2", "Grok 2", false),
];

#[derive(Debug, Clone)]
pub struct CatalogModel {
    pub info: ModelInfo,
    /// Preferred chat base, e.g. `https://cli-chat-proxy.grok.com/v1` or `https://api.x.ai/v1`.
    pub base_url: Option<String>,
    /// Wire model id if different from catalog id (usually same).
    pub wire_model: String,
}

fn models_cache_path() -> PathBuf {
    grok_home().join("models_cache.json")
}

fn config_toml_path() -> PathBuf {
    grok_home().join("config.toml")
}

/// Default model id: config.toml → newest from cache → `grok-build`.
pub fn resolve_default_model() -> String {
    if let Some(d) = read_config_default() {
        return d;
    }
    let catalog = load_catalog();
    // Prefer highest "latest" looking id: grok-4.5 > grok-build > grok-4 > …
    pick_preferred_default(&catalog).unwrap_or_else(|| "grok-build".into())
}

fn read_config_default() -> Option<String> {
    let text = fs::read_to_string(config_toml_path()).ok()?;
    // Minimal TOML scan — avoid pulling a toml crate for one key.
    // Match: under [models] (or [model] typo) a line `default = "…"`
    let mut in_models = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_models = t == "[models]";
            continue;
        }
        if !in_models {
            continue;
        }
        if let Some(rest) = t.strip_prefix("default") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let v = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn pick_preferred_default(catalog: &[CatalogModel]) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    // Explicit preference order for coding agent defaults.
    const PREFER: &[&str] = &[
        "grok-build",
        "grok-4.5",
        "grok-4",
        "grok-3",
        "grok-composer-2.5-fast",
    ];
    for id in PREFER {
        if catalog.iter().any(|m| m.info.id == *id) {
            return Some((*id).to_string());
        }
    }
    // Otherwise: sort by id descending (often encodes version) and take first.
    let mut ids: Vec<_> = catalog.iter().map(|m| m.info.id.clone()).collect();
    ids.sort_by(|a, b| b.cmp(a));
    ids.into_iter().next()
}

/// Full catalog for the model picker (cache + builtins, de-duped).
pub fn load_catalog() -> Vec<CatalogModel> {
    let mut by_id: BTreeMap<String, CatalogModel> = BTreeMap::new();

    // Builtins first (lower priority).
    for (id, name, effort) in BUILTIN {
        by_id.insert(
            (*id).to_string(),
            CatalogModel {
                info: ModelInfo {
                    id: (*id).to_string(),
                    display_name: (*name).to_string(),
                    supports_effort: *effort,
                },
                base_url: None,
                wire_model: (*id).to_string(),
            },
        );
    }

    // Cache overwrites / extends with server truth.
    if let Some(cached) = read_models_cache() {
        for m in cached {
            by_id.insert(m.info.id.clone(), m);
        }
    }

    let mut out: Vec<_> = by_id.into_values().collect();
    // Stable UI order: preferred coding models first, then alpha.
    out.sort_by(|a, b| {
        let ra = rank(&a.info.id);
        let rb = rank(&b.info.id);
        ra.cmp(&rb).then_with(|| a.info.id.cmp(&b.info.id))
    });
    out
}

fn rank(id: &str) -> u8 {
    match id {
        "grok-build" => 0,
        "grok-4.5" => 1,
        "grok-4" => 2,
        "grok-composer-2.5-fast" => 3,
        "grok-3" => 4,
        "grok-3-mini" => 5,
        "grok-2" => 6,
        _ => 50,
    }
}

pub fn lookup(id: &str) -> Option<CatalogModel> {
    load_catalog().into_iter().find(|m| m.info.id == id)
}

#[derive(Debug, Deserialize)]
struct CacheRoot {
    models: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Deserialize)]
struct CacheEntry {
    info: CacheInfo,
}

#[derive(Debug, Deserialize)]
struct CacheInfo {
    id: Option<String>,
    model: Option<String>,
    name: Option<String>,
    base_url: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    supports_reasoning_effort: bool,
    supported_in_api: Option<bool>,
}

fn read_models_cache() -> Option<Vec<CatalogModel>> {
    let raw = fs::read_to_string(models_cache_path()).ok()?;
    let root: CacheRoot = serde_json::from_str(&raw).ok()?;
    let mut out = Vec::new();
    for (key, entry) in root.models {
        if entry.info.hidden {
            continue;
        }
        // Prefer models the server marks as API-supported; keep unknowns.
        if entry.info.supported_in_api == Some(false) {
            continue;
        }
        let id = entry
            .info
            .id
            .clone()
            .or_else(|| entry.info.model.clone())
            .unwrap_or(key);
        let wire = entry.info.model.clone().unwrap_or_else(|| id.clone());
        let display = entry.info.name.clone().unwrap_or_else(|| id.clone());
        out.push(CatalogModel {
            info: ModelInfo {
                id: id.clone(),
                display_name: display,
                supports_effort: entry.info.supports_reasoning_effort,
            },
            base_url: entry.info.base_url,
            wire_model: wire,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_includes_grok_build() {
        let c = load_catalog();
        assert!(c.iter().any(|m| m.info.id == "grok-build"));
    }

    #[test]
    fn preferred_default_order() {
        let catalog = vec![
            CatalogModel {
                info: ModelInfo {
                    id: "grok-2".into(),
                    display_name: "Grok 2".into(),
                    supports_effort: false,
                },
                base_url: None,
                wire_model: "grok-2".into(),
            },
            CatalogModel {
                info: ModelInfo {
                    id: "grok-4.5".into(),
                    display_name: "Grok 4.5".into(),
                    supports_effort: true,
                },
                base_url: None,
                wire_model: "grok-4.5".into(),
            },
        ];
        assert_eq!(
            pick_preferred_default(&catalog).as_deref(),
            Some("grok-4.5")
        );
    }
}
