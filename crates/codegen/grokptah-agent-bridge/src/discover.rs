//! Filesystem discovery for MCP configs, skills, plugins, hooks (real paths, not hard-coded lists).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::types::{McpServerInfo, PluginInfo, SkillInfo};

/// Process-wide override for the GrokPtah data directory.
///
/// Integration tests set this so they never write into the developer's real
/// `~/.grokptah` (which previously polluted the desktop session list).
fn home_override() -> &'static Mutex<Option<PathBuf>> {
    static O: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    O.get_or_init(|| Mutex::new(None))
}

/// Set or clear the data-dir override.
///
/// Callers that run concurrent tests must serialize access (see test helpers).
pub fn set_grokptah_home_override(path: Option<PathBuf>) {
    if let Ok(mut g) = home_override().lock() {
        *g = path;
    }
}

/// Resolve the GrokPtah home directory.
///
/// Order:
/// 1. In-process override (tests)
/// 2. `GROKPTAH_HOME` env (CI / custom installs)
/// 3. `~/.grokptah`
pub fn grokptah_home() -> PathBuf {
    if let Ok(g) = home_override().lock() {
        if let Some(p) = g.as_ref() {
            return p.clone();
        }
    }
    if let Ok(p) = std::env::var("GROKPTAH_HOME") {
        let t = p.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grokptah")
}

pub fn ensure_home() -> PathBuf {
    let h = grokptah_home();
    let _ = fs::create_dir_all(&h);
    let _ = fs::create_dir_all(h.join("plugins"));
    let _ = fs::create_dir_all(h.join("skills"));
    let _ = fs::create_dir_all(h.join("sessions"));
    h
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfigFile {
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_transport() -> String {
    "stdio".into()
}
fn default_true() -> bool {
    true
}

pub fn mcp_config_paths(project: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = project {
        v.push(p.join(".mcp.json"));
        v.push(p.join(".grokptah").join("mcp.json"));
    }
    v.push(grokptah_home().join("mcp.json"));
    v
}

/// Config paths that ship with a project (not user-global `~/.grokptah/mcp.json`).
pub fn project_mcp_config_paths(project: &Path) -> Vec<PathBuf> {
    vec![
        project.join(".mcp.json"),
        project.join(".grokptah").join("mcp.json"),
    ]
}

/// True if `path` is a project-local MCP config (not under GrokPtah home).
pub fn is_project_local_mcp_config(path: &Path) -> bool {
    let home = grokptah_home();
    !path.starts_with(&home)
}

/// Whether this project root is trusted to run stdio MCP servers declared
/// in repo-local configs (`.mcp.json` / `.grokptah/mcp.json`).
pub fn is_project_mcp_trusted(project: &Path) -> bool {
    let Ok(canon) = project.canonicalize() else {
        return false;
    };
    let key = canon.to_string_lossy().into_owned();
    load_mcp_trust_map()
        .get(&key)
        .copied()
        .unwrap_or(false)
}

/// True if the user has already answered the trust prompt (yes or no).
pub fn project_mcp_trust_decided(project: &Path) -> bool {
    let Ok(canon) = project.canonicalize() else {
        return false;
    };
    let key = canon.to_string_lossy().into_owned();
    load_mcp_trust_map().contains_key(&key)
}

/// Persist per-project MCP trust (canonical path → trusted/denied).
pub fn set_project_mcp_trusted(project: &Path, trusted: bool) -> Result<(), String> {
    ensure_home();
    let canon = project
        .canonicalize()
        .map_err(|e| format!("canonicalize project for MCP trust: {e}"))?;
    let key = canon.to_string_lossy().into_owned();
    let mut map = load_mcp_trust_map();
    map.insert(key, trusted);
    save_mcp_trust_map(&map)
}

/// True if any project-local MCP config file exists and declares servers.
pub fn project_has_local_mcp_servers(project: &Path) -> bool {
    for path in project_mcp_config_paths(project) {
        if !path.exists() {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<McpConfigFile>(&raw) {
                if !cfg.servers.is_empty() {
                    return true;
                }
            } else if let Ok(list) = serde_json::from_str::<Vec<McpServerConfig>>(&raw) {
                if !list.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct McpTrustFile {
    /// Canonical project roots that may run project-declared stdio MCP servers.
    #[serde(default)]
    projects: std::collections::HashMap<String, bool>,
}

fn mcp_trust_path() -> PathBuf {
    grokptah_home().join("mcp_trust.json")
}

fn load_mcp_trust_map() -> std::collections::HashMap<String, bool> {
    let path = mcp_trust_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return std::collections::HashMap::new();
    };
    serde_json::from_str::<McpTrustFile>(&raw)
        .map(|f| f.projects)
        .unwrap_or_default()
}

fn save_mcp_trust_map(map: &std::collections::HashMap<String, bool>) -> Result<(), String> {
    ensure_home();
    let file = McpTrustFile {
        projects: map.clone(),
    };
    fs::write(
        mcp_trust_path(),
        serde_json::to_string_pretty(&file).unwrap_or_else(|_| "{}".into()),
    )
    .map_err(|e| e.to_string())
}

pub fn load_mcp_servers(project: Option<&Path>) -> Vec<McpServerInfo> {
    ensure_home();
    for path in mcp_config_paths(project) {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<McpConfigFile>(&raw) {
                return cfg
                    .servers
                    .into_iter()
                    .map(|s| McpServerInfo {
                        name: s.name,
                        transport: s.transport,
                        enabled: s.enabled,
                        status: if s.enabled {
                            "configured".into()
                        } else {
                            "disabled".into()
                        },
                    })
                    .collect();
            }
            // Also accept array form
            if let Ok(list) = serde_json::from_str::<Vec<McpServerConfig>>(&raw) {
                return list
                    .into_iter()
                    .map(|s| McpServerInfo {
                        name: s.name,
                        transport: s.transport,
                        enabled: s.enabled,
                        status: if s.enabled {
                            "configured".into()
                        } else {
                            "disabled".into()
                        },
                    })
                    .collect();
            }
        }
    }
    // Seed a default config file if none exists
    let seed = grokptah_home().join("mcp.json");
    if !seed.exists() {
        let default = McpConfigFile {
            servers: vec![McpServerConfig {
                name: "filesystem".into(),
                transport: "stdio".into(),
                command: Some("npx".into()),
                args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into()],
                url: None,
                enabled: false,
            }],
        };
        let _ = fs::write(
            &seed,
            serde_json::to_string_pretty(&default).unwrap_or_default(),
        );
        return load_mcp_servers(project);
    }
    Vec::new()
}

pub fn save_mcp_server_enabled(project: Option<&Path>, name: &str, enabled: bool) -> bool {
    for path in mcp_config_paths(project) {
        if !path.exists() {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(mut cfg) = serde_json::from_str::<McpConfigFile>(&raw) {
                if let Some(s) = cfg.servers.iter_mut().find(|s| s.name == name) {
                    s.enabled = enabled;
                    let _ = fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap());
                    return true;
                }
            }
        }
    }
    false
}

pub fn add_mcp_stdio(name: &str, command: &str, args: Vec<String>) -> Result<(), String> {
    ensure_home();
    let path = grokptah_home().join("mcp.json");
    let mut cfg = if path.exists() {
        let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&raw).unwrap_or(McpConfigFile { servers: vec![] })
    } else {
        McpConfigFile { servers: vec![] }
    };
    cfg.servers.retain(|s| s.name != name);
    cfg.servers.push(McpServerConfig {
        name: name.into(),
        transport: "stdio".into(),
        command: Some(command.into()),
        args,
        url: None,
        enabled: true,
    });
    fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())
}

pub fn mcp_doctor_lines(project: Option<&Path>) -> Vec<String> {
    let mut lines = Vec::new();
    for path in mcp_config_paths(project) {
        lines.push(format!(
            "config: {} ({})",
            path.display(),
            if path.exists() { "found" } else { "missing" }
        ));
    }
    if let Some(p) = project {
        let has_local = project_has_local_mcp_servers(p);
        let trusted = is_project_mcp_trusted(p);
        lines.push(format!(
            "project MCP trust: trusted={trusted} has_local_config={has_local}"
        ));
        if has_local && !trusted {
            lines.push(
                "WARNING: project-local MCP servers will NOT spawn until this project is trusted"
                    .into(),
            );
        }
    }
    for s in load_mcp_servers(project) {
        let probe = if s.transport == "stdio" {
            // Find command in config
            "stdio"
        } else {
            "http"
        };
        lines.push(format!(
            "server {} transport={} enabled={} probe={}",
            s.name, s.transport, s.enabled, probe
        ));
    }
    // Probe `npx` availability for stdio servers
    let npx_ok = std::process::Command::new("npx")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    lines.push(format!("npx available: {npx_ok}"));
    lines
}

pub fn discover_skills(project: Option<&Path>) -> Vec<SkillInfo> {
    ensure_home();
    let mut dirs = vec![grokptah_home().join("skills")];
    if let Some(p) = project {
        dirs.push(p.join(".grok").join("skills"));
        dirs.push(p.join(".claude").join("skills"));
        dirs.push(p.join("skills"));
    }
    let mut out = Vec::new();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir).max_depth(3).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skill".into());
            let desc = fs::read_to_string(path)
                .ok()
                .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(|l| l.to_string()))
                .unwrap_or_default();
            out.push(SkillInfo {
                id: path.display().to_string(),
                name,
                description: desc.chars().take(160).collect(),
            });
        }
    }
    // Seed example skill if empty
    if out.is_empty() {
        let seed = grokptah_home().join("skills").join("code-review.md");
        let _ = fs::write(
            &seed,
            "# Code Review\n\nReview local diffs and suggest improvements.\n",
        );
        return discover_skills(project);
    }
    out
}

pub fn discover_plugins() -> Vec<PluginInfo> {
    ensure_home();
    let dir = grokptah_home().join("plugins");
    let mut out = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() || p.extension().and_then(|e| e.to_str()) == Some("json") {
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push(PluginInfo {
                    id: name.clone(),
                    name: name.clone(),
                    installed: true,
                    enabled: true,
                });
            }
        }
    }
    // Marketplace catalog (local)
    let catalog = grokptah_home().join("plugin-catalog.json");
    if !catalog.exists() {
        let _ = fs::write(
            &catalog,
            r#"[
  {"id":"diff-review","name":"Diff Review","description":"Highlight agent edits"},
  {"id":"commit-helper","name":"Commit Helper","description":"Conventional commits"}
]"#,
        );
    }
    if let Ok(raw) = fs::read_to_string(&catalog) {
        if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) {
            for it in items {
                let id = it
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                if out.iter().any(|p| p.id == id) {
                    continue;
                }
                out.push(PluginInfo {
                    id: id.clone(),
                    name: it
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&id)
                        .to_string(),
                    installed: false,
                    enabled: false,
                });
            }
        }
    }
    out
}

pub fn install_plugin(id: &str) -> Result<PluginInfo, String> {
    ensure_home();
    let dest = grokptah_home().join("plugins").join(format!("{id}.json"));
    let body = serde_json::json!({
        "id": id,
        "installedAt": chrono::Utc::now().to_rfc3339(),
        "enabled": true
    });
    fs::write(&dest, serde_json::to_string_pretty(&body).unwrap()).map_err(|e| e.to_string())?;
    Ok(PluginInfo {
        id: id.into(),
        name: id.into(),
        installed: true,
        enabled: true,
    })
}

pub fn hooks_config_text(project: Option<&Path>) -> String {
    let candidates = [
        project.map(|p| p.join(".grokptah").join("hooks.json")),
        project.map(|p| p.join(".claude").join("settings.json")),
        Some(grokptah_home().join("hooks.json")),
    ];
    for c in candidates.into_iter().flatten() {
        if c.is_file() {
            return fs::read_to_string(c).unwrap_or_default();
        }
    }
    let seed = grokptah_home().join("hooks.json");
    if !seed.exists() {
        let _ = fs::write(
            &seed,
            r#"{
  "hooks": {
    "PreToolUse": [],
    "PostToolUse": []
  }
}"#,
        );
    }
    fs::read_to_string(seed).unwrap_or_else(|_| "{}".into())
}

// need chrono in discover - use crate chrono already in deps
