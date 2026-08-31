//! Local in-process tools the bridge can run without a child agent process.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use crate::events::ToolCallKind;
use crate::host_helpers::{
    bounded_tool_output_with_digests, bounded_tool_output_with_integrity, StationarityNormalizer,
};

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
    /// Process exit code for shell tools; `None` when killed by cancellation.
    pub exit_code: Option<i32>,
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
            exit_code: None,
        }
    }
}

pub fn resolve_under_cwd(cwd: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        anyhow::bail!("absolute paths are not allowed: {}", rel_path.display());
    }

    let mut normalized = PathBuf::new();
    for component in rel_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    anyhow::bail!("path escapes project root: {}", rel_path.display());
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("absolute paths are not allowed: {}", rel_path.display());
            }
        }
    }

    let canon_cwd = dunce::canonicalize(cwd)
        .with_context(|| format!("canonicalize project root {}", cwd.display()))?;
    let mut existing_ancestor = cwd.join(&normalized);
    let mut missing_parts = Vec::new();

    while std::fs::symlink_metadata(&existing_ancestor).is_err() {
        let Some(part) = existing_ancestor
            .file_name()
            .map(|part| part.to_os_string())
        else {
            anyhow::bail!("could not resolve path under project root: {rel}");
        };
        missing_parts.push(part);
        if !existing_ancestor.pop() {
            anyhow::bail!("could not resolve path under project root: {rel}");
        }
    }

    let canon_ancestor = dunce::canonicalize(&existing_ancestor)
        .with_context(|| format!("canonicalize path {}", existing_ancestor.display()))?;
    if !canon_ancestor.starts_with(&canon_cwd) {
        anyhow::bail!("path escapes project root: {}", canon_ancestor.display());
    }

    let mut resolved = canon_ancestor;
    for part in missing_parts.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

pub async fn tool_read_file(cwd: &Path, path: &str) -> Result<ToolResult> {
    let full = resolve_under_cwd(cwd, path)?;
    let text = tokio::fs::read_to_string(&full)
        .await
        .with_context(|| format!("read {}", full.display()))?;
    let truncated = bounded_tool_output_with_integrity(&text, 32_000);
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
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create parent {}", parent.display()))?;
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

/// Write many files in one tool call (turn-efficient multi-file edits for #187/#188).
pub async fn tool_write_files(cwd: &Path, files: &[(String, String)]) -> Result<ToolResult> {
    if files.is_empty() {
        anyhow::bail!("write_files requires a non-empty files array");
    }
    let mut resolved = Vec::with_capacity(files.len());
    let mut destinations = std::collections::HashSet::with_capacity(files.len());
    for (path, content) in files {
        let full = resolve_under_cwd(cwd, path)?;
        if !destinations.insert(full.clone()) {
            anyhow::bail!("write_files contains duplicate destination: {path}");
        }
        resolved.push((path, content, full));
    }

    let mut written = Vec::new();
    let mut total_bytes = 0usize;
    for (path, content, full) in resolved {
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create parent {}", parent.display()))?;
        }
        tokio::fs::write(&full, content)
            .await
            .with_context(|| format!("write {}", full.display()))?;
        total_bytes += content.len();
        written.push(path.clone());
    }
    Ok(ToolResult::basic(
        format!("Write {} files", written.len()),
        ToolCallKind::Edit,
        serde_json::json!({ "paths": written, "bytes": total_bytes }),
        format!(
            "Wrote {} file(s) ({} bytes total): {}",
            written.len(),
            total_bytes,
            written.join(", ")
        ),
        true,
        format!("write_files {}", written.join(",")),
    ))
}

/// Per-session live shell children so concurrent sessions do not kill each other.
pub type LiveShellMap = Arc<TokioMutex<std::collections::HashMap<uuid::Uuid, Child>>>;

/// Run a shell command with streamed stdout/stderr, cancellable via `cancel`
/// (kills the child process — works for any command, not only sleep).
pub async fn tool_shell_streaming<F>(
    cwd: &Path,
    command: &str,
    cancel: CancellationToken,
    session_id: uuid::Uuid,
    live_shells: LiveShellMap,
    mut on_chunk: F,
) -> Result<ToolResult>
where
    F: FnMut(String) + Send,
{
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::spawn_env::scrub_tokio_command(&mut cmd);
    crate::process_tree::configure(&mut cmd);
    let mut child = cmd.spawn().context("spawn shell")?;

    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;

    {
        let mut map = live_shells.lock().await;
        // Replace only this session's previous child (if any).
        if let Some(mut old) = map.remove(&session_id) {
            crate::process_tree::terminate(&mut old).await;
        }
        map.insert(session_id, child);
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
    let mut full_digest = Sha256::new();
    let mut stable_digest = StationarityNormalizer::default();
    let mut full_len = 0usize;
    let mut cancelled = false;
    let mut exit_code: Option<i32> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                cancelled = true;
                let mut map = live_shells.lock().await;
                if let Some(mut child) = map.remove(&session_id) {
                    crate::process_tree::terminate(&mut child).await;
                }
                break;
            }
            msg = chunk_rx.recv() => {
                match msg {
                    Some(s) => {
                        full_digest.update(s.as_bytes());
                        stable_digest.feed(&s);
                        full_len = full_len.saturating_add(s.len());
                        if collected.len() < 32_000 {
                            let remaining = 32_000 - collected.len();
                            let visible = crate::textutil::truncate_at_char_boundary(&s, remaining);
                            collected.push_str(visible);
                            if !visible.is_empty() {
                                on_chunk(visible.to_owned());
                            }
                        }
                    }
                    None => {
                        // both pipes closed — wait for child
                        let mut map = live_shells.lock().await;
                        if let Some(mut child) = map.remove(&session_id) {
                            if let Ok(status) = child.wait().await {
                                exit_code = status.code();
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
        full_digest.update(s.as_bytes());
        stable_digest.feed(&s);
        full_len = full_len.saturating_add(s.len());
        if collected.len() < 32_000 {
            let remaining = 32_000 - collected.len();
            let visible = crate::textutil::truncate_at_char_boundary(&s, remaining);
            collected.push_str(visible);
            if !visible.is_empty() {
                on_chunk(visible.to_owned());
            }
        }
    }

    // Ensure this session's slot cleared
    {
        let mut map = live_shells.lock().await;
        if let Some(mut child) = map.remove(&session_id) {
            if cancelled {
                crate::process_tree::terminate(&mut child).await;
            } else if let Ok(status) = child.wait().await {
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
    } else {
        let exit_summary = exit_code
            .map(|code| format!("(exit {code})"))
            .unwrap_or_else(|| "(terminated without exit code)".into());
        if collected.is_empty() {
            exit_summary
        } else {
            format!("{}\n{exit_summary}", collected.trim_end())
        }
    };

    let status_suffix = if cancelled {
        if full_len == 0 {
            "(cancelled)".to_owned()
        } else {
            "\n(cancelled)".to_owned()
        }
    } else {
        let exit_summary = exit_code
            .map(|code| format!("(exit {code})"))
            .unwrap_or_else(|| "(terminated without exit code)".into());
        if full_len == 0 {
            exit_summary
        } else {
            format!("\n{exit_summary}")
        }
    };
    full_digest.update(status_suffix.as_bytes());
    stable_digest.feed(&status_suffix);
    let total_len = full_len.saturating_add(status_suffix.len());
    let raw_digest = full_digest.finalize();
    let stable_digest = stable_digest.finish();
    let display = bounded_tool_output_with_digests(
        &output,
        total_len,
        raw_digest.as_ref(),
        stable_digest.as_ref(),
        32_000,
    );

    Ok(ToolResult {
        title: format!("$ {command}"),
        kind: ToolCallKind::Execute,
        input: serde_json::json!({ "command": command }),
        output: display,
        needs_permission: true,
        permission_summary: format!("Run shell: {command}"),
        cancelled,
        exit_code,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_files_batch_writes_all_paths() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![
            ("src/a.rs".into(), "pub fn a() {}\n".into()),
            ("src/b.rs".into(), "pub fn b() {}\n".into()),
            ("src/c.rs".into(), "pub fn c() {}\n".into()),
        ];
        let r = tool_write_files(dir.path(), &files).await.unwrap();
        assert!(r.output.contains("3 file"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/a.rs")).unwrap(),
            "pub fn a() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/b.rs")).unwrap(),
            "pub fn b() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/c.rs")).unwrap(),
            "pub fn c() {}\n"
        );
    }

    #[tokio::test]
    async fn write_files_empty_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let res = tool_write_files(dir.path(), &[]).await;
        assert!(res.is_err());
        assert!(
            res.err().unwrap().to_string().contains("non-empty"),
            "empty files should error"
        );
    }

    #[tokio::test]
    async fn write_file_rejects_new_target_outside_project() {
        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().join("project");
        std::fs::create_dir(&project).unwrap();

        let result = tool_write_file(&project, "../outside.txt", "nope").await;

        assert!(result.is_err());
        assert!(!parent.path().join("outside.txt").exists());
    }

    #[tokio::test]
    async fn write_files_preflights_entire_batch_before_writing() {
        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let files = vec![
            ("inside.txt".into(), "would be partial".into()),
            ("../outside.txt".into(), "nope".into()),
        ];

        let result = tool_write_files(&project, &files).await;

        assert!(result.is_err());
        assert!(!project.join("inside.txt").exists());
        assert!(!parent.path().join("outside.txt").exists());
    }

    #[tokio::test]
    async fn write_file_allows_normalized_nested_target_inside_project() {
        let project = tempfile::tempdir().unwrap();

        tool_write_file(
            project.path(),
            "src/generated/../generated/value.rs",
            "pub const VALUE: u8 = 1;\n",
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(project.path().join("src/generated/value.rs")).unwrap(),
            "pub const VALUE: u8 = 1;\n"
        );
    }

    #[tokio::test]
    async fn write_file_rejects_absolute_target() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let original = std::fs::read(outside.path()).unwrap();

        let result =
            tool_write_file(project.path(), &outside.path().to_string_lossy(), "nope").await;

        assert!(result.is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), original);
    }

    #[tokio::test]
    async fn read_file_integrity_marker_distinguishes_suffix_beyond_cap() {
        let project = tempfile::tempdir().unwrap();
        let first = format!("{}A", "x".repeat(40_000));
        let second = format!("{}B", "x".repeat(40_000));
        std::fs::write(project.path().join("first.txt"), first).unwrap();
        std::fs::write(project.path().join("second.txt"), second).unwrap();

        let first = tool_read_file(project.path(), "first.txt").await.unwrap();
        let second = tool_read_file(project.path(), "second.txt").await.unwrap();

        assert!(first.output.len() <= 32_000);
        assert!(first.output.starts_with("[grokptah-output-integrity-v1"));
        assert_ne!(first.output, second.output);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_rejects_symlinked_parent_outside_project() {
        use std::os::unix::fs::symlink;

        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), project.path().join("linked")).unwrap();

        let result = tool_write_file(project.path(), "linked/escaped.txt", "nope").await;

        assert!(result.is_err());
        assert!(!outside.path().join("escaped.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_dangling_symlink_escape() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().join("project");
        std::fs::create_dir(&project).unwrap();
        symlink(
            parent.path().join("missing-outside"),
            project.join("dangling"),
        )
        .unwrap();

        let result = resolve_under_cwd(&project, "dangling/escaped.txt");

        assert!(result.is_err());
    }

    fn shell_map() -> LiveShellMap {
        Arc::new(TokioMutex::new(std::collections::HashMap::new()))
    }

    #[tokio::test]
    async fn shell_reports_zero_exit_code() {
        let project = tempfile::tempdir().unwrap();
        let result = tool_shell_streaming(
            project.path(),
            "printf success",
            CancellationToken::new(),
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert_eq!(result.output, "success\n(exit 0)");
    }

    #[tokio::test]
    async fn shell_reports_nonzero_exit_code() {
        let project = tempfile::tempdir().unwrap();
        let result = tool_shell_streaming(
            project.path(),
            "printf failure; exit 2",
            CancellationToken::new(),
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, Some(2));
        assert!(!result.cancelled);
        assert_eq!(result.output, "failure\n(exit 2)");
    }

    #[tokio::test]
    async fn shell_integrity_marker_survives_stream_cap_and_suffix_changes() {
        let project = tempfile::tempdir().unwrap();
        let first = tool_shell_streaming(
            project.path(),
            "head -c 40000 /dev/zero | tr '\\0' a; printf A",
            CancellationToken::new(),
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();
        let second = tool_shell_streaming(
            project.path(),
            "head -c 40000 /dev/zero | tr '\\0' a; printf B",
            CancellationToken::new(),
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();

        assert!(first.output.len() <= 32_000);
        assert!(first.output.starts_with("[grokptah-output-integrity-v1"));
        assert_ne!(first.output, second.output);
    }

    #[tokio::test]
    async fn shell_integrity_marker_normalizes_volatile_metadata_streaming() {
        let project = tempfile::tempdir().unwrap();
        let first = tool_shell_streaming(
            project.path(),
            "printf 'timestamp=2026-08-30T21:00:00Z pid=1201 request_id=11111111-1111-1111-1111-111111111111 '; head -c 40000 /dev/zero | tr '\\0' a",
            CancellationToken::new(),
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();
        let second = tool_shell_streaming(
            project.path(),
            "printf 'timestamp=2026-08-30T21:01:00Z pid=1202 request_id=22222222-2222-2222-2222-222222222222 '; head -c 40000 /dev/zero | tr '\\0' a",
            CancellationToken::new(),
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();

        let first_marker = first.output.lines().next().unwrap();
        let second_marker = second.output.lines().next().unwrap();
        assert_ne!(first_marker, second_marker);
        let first_stable = first_marker.split(" stable_sha256=").nth(1).unwrap();
        let second_stable = second_marker.split(" stable_sha256=").nth(1).unwrap();
        assert_eq!(first_stable, second_stable);
    }

    #[tokio::test]
    async fn shell_cancellation_is_distinct_from_process_failure() {
        let project = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool_shell_streaming(
            project.path(),
            "sleep 5",
            cancel,
            uuid::Uuid::new_v4(),
            shell_map(),
            |_| {},
        )
        .await
        .unwrap();

        assert!(result.cancelled);
        assert_eq!(result.exit_code, None);
        assert!(result.output.contains("(cancelled)"));
    }
}
