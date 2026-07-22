//! Agents + personas discovery (#164).
//!
//! Agents: `.md` under project/user agent dirs (Build-compatible paths).
//! Personas: `.toml` or simple key=value under personas dirs.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discover::grokptah_home;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// Absolute path to the agent markdown (if any).
    pub path: String,
    /// Body text (instructions).
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaDef {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub path: String,
}

fn agent_dirs(project: Option<&Path>) -> Vec<PathBuf> {
    let mut d = Vec::new();
    if let Some(p) = project {
        d.push(p.join(".grok").join("agents"));
        d.push(p.join(".grokptah").join("agents"));
        d.push(p.join("agents"));
    }
    d.push(grokptah_home().join("agents"));
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    d.push(home.join(".grok").join("agents"));
    d
}

fn persona_dirs(project: Option<&Path>) -> Vec<PathBuf> {
    let mut d = Vec::new();
    if let Some(p) = project {
        d.push(p.join(".grok").join("personas"));
        d.push(p.join(".grokptah").join("personas"));
    }
    d.push(grokptah_home().join("personas"));
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    d.push(home.join(".grok").join("personas"));
    d
}

/// Discover agent definition markdown files.
pub fn discover_agents(project: Option<&Path>) -> Vec<AgentDef> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in agent_dirs(project) {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("agent")
                .to_string();
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            let body = fs::read_to_string(&p).unwrap_or_default();
            let description = first_paragraph(&body);
            out.push(AgentDef {
                name,
                description,
                path: p.display().to_string(),
                body,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Discover persona definitions (`.toml` with `instructions = "..."` or plain text `.md`).
pub fn discover_personas(project: Option<&Path>) -> Vec<PersonaDef> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in persona_dirs(project) {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
            if ext != "toml" && ext != "md" {
                continue;
            }
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("persona")
                .to_string();
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            let raw = fs::read_to_string(&p).unwrap_or_default();
            let instructions = if ext == "toml" {
                parse_toml_instructions(&raw).unwrap_or(raw.clone())
            } else {
                raw.clone()
            };
            let description = first_paragraph(&instructions);
            out.push(PersonaDef {
                name,
                description,
                instructions,
                path: p.display().to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn first_paragraph(body: &str) -> String {
    let t = body.trim();
    if t.is_empty() {
        return String::new();
    }
    // Skip front matter / heading
    let mut lines = t.lines().skip_while(|l| {
        let s = l.trim();
        s.is_empty() || s.starts_with('#') || s.starts_with("---")
    });
    let mut out = String::new();
    for l in lines.by_ref() {
        let s = l.trim();
        if s.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(s);
        if out.len() > 200 {
            break;
        }
    }
    if out.len() > 160 {
        crate::textutil::truncate_with_marker(&out, 160, "…")
    } else {
        out
    }
}

fn parse_toml_instructions(raw: &str) -> Option<String> {
    // Minimal: instructions = """...""" or instructions = "..."
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("instructions") {
            let rest = rest.trim().strip_prefix('=')?.trim();
            if let Some(s) = rest.strip_prefix("\"\"\"") {
                // multiline start — grab until closing
                let mut body = String::new();
                if let Some(end) = s.find("\"\"\"") {
                    return Some(s[..end].to_string());
                }
                body.push_str(s);
                body.push('\n');
                let mut started = true;
                for l in raw.lines().skip_while(|l| !l.contains("instructions")) {
                    if !started {
                        continue;
                    }
                    if l.contains("\"\"\"") && !l.trim().starts_with("instructions") {
                        let part = l.split("\"\"\"").next().unwrap_or("");
                        body.push_str(part);
                        return Some(body);
                    }
                    if l.trim().starts_with("instructions") {
                        started = true;
                        continue;
                    }
                    body.push_str(l);
                    body.push('\n');
                }
                return Some(body);
            }
            if let Some(s) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                return Some(s.to_string());
            }
        }
    }
    // description = optional
    None
}

/// Resolve persona instructions by name (case-insensitive).
pub fn resolve_persona(project: Option<&Path>, name: &str) -> Option<PersonaDef> {
    let n = name.trim().to_ascii_lowercase();
    discover_personas(project)
        .into_iter()
        .find(|p| p.name.to_ascii_lowercase() == n)
}

/// Resolve agent definition by name.
pub fn resolve_agent(project: Option<&Path>, name: &str) -> Option<AgentDef> {
    let n = name.trim().to_ascii_lowercase();
    discover_agents(project)
        .into_iter()
        .find(|a| a.name.to_ascii_lowercase() == n)
}

/// System-reminder style block for a persona (Build-compatible shape).
pub fn persona_system_reminder(p: &PersonaDef) -> String {
    format!(
        "<system-reminder>\nPersona `{}`: {}\n\n{}\n</system-reminder>",
        p.name,
        p.description,
        p.instructions.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_agent_and_persona_files() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join(".grok").join("agents");
        let personas = dir.path().join(".grok").join("personas");
        fs::create_dir_all(&agents).unwrap();
        fs::create_dir_all(&personas).unwrap();
        fs::write(
            agents.join("reviewer.md"),
            "# Reviewer\n\nAlways cite file paths.\n",
        )
        .unwrap();
        fs::write(
            personas.join("concise.toml"),
            r#"description = "Short answers"
instructions = "Be brief. Prefer bullets."
"#,
        )
        .unwrap();
        let a = discover_agents(Some(dir.path()));
        assert!(a.iter().any(|x| x.name == "reviewer"), "{a:?}");
        let p = discover_personas(Some(dir.path()));
        assert!(p.iter().any(|x| x.name == "concise"), "{p:?}");
        let r = resolve_persona(Some(dir.path()), "concise").unwrap();
        assert!(r.instructions.contains("brief") || r.instructions.contains("Be brief"));
    }
}
