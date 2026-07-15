//! Local in-process tools the bridge can run without a child agent process.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;
use walkdir::WalkDir;

use crate::events::ToolCallKind;

#[allow(dead_code)]
pub struct ToolResult {
    pub title: String,
    pub kind: ToolCallKind,
    pub input: serde_json::Value,
    pub output: String,
    pub needs_permission: bool,
    pub permission_summary: String,
}

pub fn resolve_under_cwd(cwd: &Path, rel: &str) -> Result<PathBuf> {
    let p = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        cwd.join(rel)
    };
    let canon_cwd = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf());
    // Best-effort containment: if path exists, canonicalize; else check parent.
    if p.exists() {
        let c = p.canonicalize().context("canonicalize path")?;
        if !c.starts_with(&canon_cwd) {
            anyhow::bail!("path escapes project root: {}", c.display());
        }
        Ok(c)
    } else {
        Ok(p)
    }
}

pub async fn tool_read_file(cwd: &Path, path: &str) -> Result<ToolResult> {
    let full = resolve_under_cwd(cwd, path)?;
    let text = tokio::fs::read_to_string(&full)
        .await
        .with_context(|| format!("read {}", full.display()))?;
    let truncated = if text.len() > 32_000 {
        format!("{}\n… (truncated)", &text[..32_000])
    } else {
        text
    };
    Ok(ToolResult {
        title: format!("Read {}", path),
        kind: ToolCallKind::Read,
        input: serde_json::json!({ "path": path }),
        output: truncated,
        needs_permission: false,
        permission_summary: String::new(),
    })
}

pub async fn tool_list_dir(cwd: &Path, path: &str) -> Result<ToolResult> {
    let full = resolve_under_cwd(cwd, if path.is_empty() { "." } else { path })?;
    let mut entries = Vec::new();
    let mut rd = tokio::fs::read_dir(&full).await?;
    while let Some(e) = rd.next_entry().await? {
        let name = e.file_name().to_string_lossy().into_owned();
        let ty = if e.file_type().await?.is_dir() {
            "dir"
        } else {
            "file"
        };
        entries.push(format!("{ty}\t{name}"));
    }
    entries.sort();
    Ok(ToolResult {
        title: format!("List {}", path),
        kind: ToolCallKind::Read,
        input: serde_json::json!({ "path": path }),
        output: entries.join("\n"),
        needs_permission: false,
        permission_summary: String::new(),
    })
}

pub async fn tool_grep(cwd: &Path, pattern: &str, path: &str) -> Result<ToolResult> {
    let re = regex::Regex::new(pattern).context("invalid regex")?;
    let root = resolve_under_cwd(cwd, if path.is_empty() { "." } else { path })?;
    let mut hits = Vec::new();
    for entry in WalkDir::new(&root).max_depth(6).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e,
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bin" | "exe" | "o" | "a"
            )
        }) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(p) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                let rel = p.strip_prefix(cwd).unwrap_or(p);
                hits.push(format!("{}:{}:{}", rel.display(), i + 1, line.trim()));
                if hits.len() >= 50 {
                    break;
                }
            }
        }
        if hits.len() >= 50 {
            break;
        }
    }
    Ok(ToolResult {
        title: format!("Search /{pattern}/"),
        kind: ToolCallKind::Search,
        input: serde_json::json!({ "pattern": pattern, "path": path }),
        output: if hits.is_empty() {
            "(no matches)".into()
        } else {
            hits.join("\n")
        },
        needs_permission: false,
        permission_summary: String::new(),
    })
}

#[allow(dead_code)]
pub async fn tool_write_file(cwd: &Path, path: &str, content: &str) -> Result<ToolResult> {
    let full = resolve_under_cwd(cwd, path)?;
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&full, content)
        .await
        .with_context(|| format!("write {}", full.display()))?;
    Ok(ToolResult {
        title: format!("Write {}", path),
        kind: ToolCallKind::Edit,
        input: serde_json::json!({ "path": path, "bytes": content.len() }),
        output: format!("Wrote {} bytes to {}", content.len(), path),
        needs_permission: true,
        permission_summary: format!("Write file {path}"),
    })
}

pub async fn tool_shell(cwd: &Path, command: &str) -> Result<ToolResult> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("spawn shell")?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr);
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if text.len() > 32_000 {
        text.truncate(32_000);
        text.push_str("\n… (truncated)");
    }
    Ok(ToolResult {
        title: format!("$ {command}"),
        kind: ToolCallKind::Execute,
        input: serde_json::json!({ "command": command }),
        output: if text.is_empty() {
            format!("(exit {})", output.status.code().unwrap_or(-1))
        } else {
            text
        },
        needs_permission: true,
        permission_summary: format!("Run shell: {command}"),
    })
}

/// Fuzzy-ish file open: match path components against query.
pub fn fuzzy_files(cwd: &Path, query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    for entry in WalkDir::new(cwd)
        .max_depth(8)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(cwd)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        if q.is_empty() || rel.to_lowercase().contains(&q) {
            out.push(rel);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

pub fn list_tree(cwd: &Path, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for entry in WalkDir::new(cwd)
        .max_depth(4)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = entry
            .path()
            .strip_prefix(cwd)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        if rel.is_empty() {
            continue;
        }
        let suffix = if entry.file_type().is_dir() { "/" } else { "" };
        out.push(format!("{rel}{suffix}"));
        if out.len() >= max {
            break;
        }
    }
    out
}
