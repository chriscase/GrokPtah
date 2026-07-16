//! Local in-process tools the bridge can run without a child agent process.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use crate::events::ToolCallKind;

#[allow(dead_code)] // fields kept for API symmetry / future UI binding
pub struct ToolResult {
    pub title: String,
    pub kind: ToolCallKind,
    pub input: serde_json::Value,
    pub output: String,
    pub needs_permission: bool,
    pub permission_summary: String,
    /// True when the tool was stopped by cancellation.
    pub cancelled: bool,
}

impl ToolResult {
    pub fn basic(
        title: String,
        kind: ToolCallKind,
        input: serde_json::Value,
        output: String,
        needs_permission: bool,
        permission_summary: String,
    ) -> Self {
        Self {
            title,
            kind,
            input,
            output,
            needs_permission,
            permission_summary,
            cancelled: false,
        }
    }
}

pub fn resolve_under_cwd(cwd: &Path, rel: &str) -> Result<PathBuf> {
    let p = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        cwd.join(rel)
    };
    let canon_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
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
    Ok(ToolResult::basic(
        format!("Read {path}"),
        ToolCallKind::Read,
        serde_json::json!({ "path": path }),
        truncated,
        false,
        String::new(),
    ))
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
    Ok(ToolResult::basic(
        format!("List {path}"),
        ToolCallKind::Read,
        serde_json::json!({ "path": path }),
        entries.join("\n"),
        false,
        String::new(),
    ))
}

pub async fn tool_grep(cwd: &Path, pattern: &str, path: &str) -> Result<ToolResult> {
    let re = regex::Regex::new(pattern).context("invalid regex")?;
    let root = resolve_under_cwd(cwd, if path.is_empty() { "." } else { path })?;
    let mut hits = Vec::new();
    for entry in WalkDir::new(&root)
        .max_depth(6)
        .into_iter()
        .filter_map(|e| e.ok())
    {
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
    Ok(ToolResult::basic(
        format!("Search /{pattern}/"),
        ToolCallKind::Search,
        serde_json::json!({ "pattern": pattern, "path": path }),
        if hits.is_empty() {
            "(no matches)".into()
        } else {
            hits.join("\n")
        },
        false,
        String::new(),
    ))
}

pub async fn tool_write_file(cwd: &Path, path: &str, content: &str) -> Result<ToolResult> {
    let full = resolve_under_cwd(cwd, path)?;
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&full, content)
        .await
        .with_context(|| format!("write {}", full.display()))?;
    Ok(ToolResult::basic(
        format!("Write {path}"),
        ToolCallKind::Edit,
        serde_json::json!({ "path": path, "bytes": content.len() }),
        format!("Wrote {} bytes to {path}", content.len()),
        true,
        format!("Write file {path}"),
    ))
}

/// Slot for the live shell child so [`crate::host::AgentHostHandle::cancel_turn`] can kill it.
pub type LiveShellSlot = Arc<TokioMutex<Option<Child>>>;

/// Run a shell command with streamed stdout/stderr, cancellable via `cancel`
/// (kills the child process — works for any command, not only sleep).
pub async fn tool_shell_streaming<F>(
    cwd: &Path,
    command: &str,
    cancel: CancellationToken,
    live_shell: LiveShellSlot,
    mut on_chunk: F,
) -> Result<ToolResult>
where
    F: FnMut(String) + Send,
{
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn shell")?;

    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;

    {
        let mut slot = live_shell.lock().await;
        *slot = Some(child);
    }

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let tx_out = chunk_tx.clone();
    let tx_err = chunk_tx;
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if tx_out.send(s).is_err() {
                        break;
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if tx_err.send(s).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut collected = String::new();
    let mut cancelled = false;
    let mut exit_code: Option<i32> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                cancelled = true;
                let mut slot = live_shell.lock().await;
                if let Some(mut child) = slot.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                break;
            }
            msg = chunk_rx.recv() => {
                match msg {
                    Some(s) => {
                        collected.push_str(&s);
                        if collected.len() > 32_000 {
                            collected.truncate(32_000);
                            collected.push_str("\n… (truncated)");
                        } else {
                            on_chunk(s);
                        }
                    }
                    None => {
                        // both pipes closed — wait for child
                        let mut slot = live_shell.lock().await;
                        if let Some(mut child) = slot.take() {
                            match child.wait().await {
                                Ok(status) => exit_code = status.code(),
                                Err(_) => {}
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    // Drain any remaining buffered chunks without blocking forever
    while let Ok(s) = chunk_rx.try_recv() {
        collected.push_str(&s);
        on_chunk(s);
    }

    // Ensure slot cleared
    {
        let mut slot = live_shell.lock().await;
        if let Some(mut child) = slot.take() {
            if cancelled {
                let _ = child.kill().await;
            }
            if let Ok(status) = child.wait().await {
                exit_code = status.code();
            }
        }
    }

    let output = if cancelled {
        if collected.is_empty() {
            "(cancelled)".into()
        } else {
            format!("{collected}\n(cancelled)")
        }
    } else if collected.is_empty() {
        format!("(exit {})", exit_code.unwrap_or(-1))
    } else {
        collected
    };

    Ok(ToolResult {
        title: format!("$ {command}"),
        kind: ToolCallKind::Execute,
        input: serde_json::json!({ "command": command }),
        output,
        needs_permission: true,
        permission_summary: format!("Run shell: {command}"),
        cancelled,
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
