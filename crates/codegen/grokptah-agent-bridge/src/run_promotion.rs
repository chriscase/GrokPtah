//! Strict Git-backed isolation and promotion for desktop Build runs.
//!
//! This module deliberately has a narrower contract than the subagent
//! isolation helper. Promotable runs start from a clean Git workspace, keep a
//! detached worktree under the project, and promote only a reviewed diff when
//! the source fingerprint is unchanged.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::orchestration::{safe_id_filename, ChangeRecord};

const MAX_PATCH_BYTES: usize = 32 * 1024 * 1024;
const MAX_REVIEW_BYTES: usize = 200 * 1024;
const MAX_CHANGED_FILES: usize = 2_000;

#[derive(Debug, Clone)]
pub(crate) struct PreparedRunWorkspace {
    pub cwd: PathBuf,
    pub base_revision: String,
    pub source_fingerprint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorktreeSnapshot {
    pub fingerprint: String,
    pub changed_files: Vec<ChangeRecord>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReview {
    pub changed_files: Vec<ChangeRecord>,
    pub diff: String,
    pub diff_truncated: bool,
    pub fingerprint: String,
}

pub(crate) fn prepare(project: &Path, run_id: &str) -> Result<PreparedRunWorkspace> {
    let source = dunce::canonicalize(project).context("canonicalize isolated source workspace")?;
    if !source.is_dir() {
        bail!(
            "isolated source workspace is not a directory: {}",
            source.display()
        );
    }

    let base_revision = git_stdout(&source, &["rev-parse", "HEAD"])?;
    let status = git_stdout_bytes(
        &source,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.is_empty() {
        bail!(
            "isolated promotable runs require a clean Git workspace; commit or stash current changes first"
        );
    }
    let source_fingerprint = fingerprint_at(&source, &base_revision)?;

    let managed_root = source.join(".grokptah").join("worktrees").join("runs");
    fs::create_dir_all(&managed_root).context("create managed run worktree directory")?;
    let managed_root = dunce::canonicalize(&managed_root)
        .context("canonicalize managed run worktree directory")?;
    if !managed_root.starts_with(&source) {
        bail!("managed run worktree directory escapes the source workspace");
    }
    let name = safe_id_filename(run_id).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let cwd = managed_root.join(format!("run-{name}"));
    if cwd.exists() {
        bail!("isolated run worktree already exists");
    }

    let output = git_command(
        &source,
        &["worktree", "add", "--detach"],
        &[cwd.as_os_str(), std::ffi::OsStr::new(&base_revision)],
        None,
    )?;
    if !output.status.success() || !cwd.join(".git").is_file() {
        bail!(
            "create isolated run worktree failed: {}",
            command_error(&output)
        );
    }
    Ok(PreparedRunWorkspace {
        cwd,
        base_revision,
        source_fingerprint,
    })
}

pub(crate) fn snapshot(worktree: &Path, base_revision: &str) -> Result<WorktreeSnapshot> {
    validate_worktree_path(worktree)?;
    ensure_intent_to_add(worktree)?;
    let changed_files = changed_files(worktree, base_revision)?;
    let fingerprint = fingerprint_at(worktree, base_revision)?;
    Ok(WorktreeSnapshot {
        fingerprint,
        changed_files,
    })
}

pub(crate) fn review(worktree: &Path, base_revision: &str) -> Result<RunReview> {
    validate_worktree_path(worktree)?;
    ensure_intent_to_add(worktree)?;
    let patch = diff_bytes(worktree, base_revision)?;
    let fingerprint = fingerprint_at(worktree, base_revision)?;
    let (diff, diff_truncated) = bounded_text(&patch, MAX_REVIEW_BYTES);
    Ok(RunReview {
        changed_files: changed_files(worktree, base_revision)?,
        diff,
        diff_truncated,
        fingerprint,
    })
}

/// Validate that a persisted execution path belongs to this source workspace.
/// Durable records are treated as untrusted input at this boundary.
pub(crate) fn validate_managed_worktree(source: &Path, worktree: &Path) -> Result<()> {
    let source = dunce::canonicalize(source).context("canonicalize promotion source")?;
    let worktree = dunce::canonicalize(worktree).context("canonicalize run worktree")?;
    let managed_root = dunce::canonicalize(source.join(".grokptah").join("worktrees").join("runs"))
        .context("canonicalize managed run worktree directory")?;
    if !managed_root.starts_with(&source) || !worktree.starts_with(&managed_root) {
        bail!("isolated run path is outside the source workspace run directory");
    }
    validate_worktree_path(&worktree)
}

/// Apply the isolated diff only when the source still matches the recorded
/// clean baseline. A matching final fingerprint makes retries idempotent,
/// including recovery after a process exit after Git applied the patch.
pub(crate) fn promote(
    source: &Path,
    worktree: &Path,
    base_revision: &str,
    source_fingerprint: &str,
    final_fingerprint: &str,
) -> Result<PromotionOutcome> {
    validate_worktree_path(worktree)?;
    let current = fingerprint_at(source, base_revision)?;
    if current == final_fingerprint {
        return Ok(PromotionOutcome::AlreadyApplied);
    }
    if current != source_fingerprint {
        bail!("source workspace changed since isolated run started; promotion refused");
    }

    let review = review(worktree, base_revision)?;
    if review.fingerprint != final_fingerprint {
        bail!("isolated worktree changed after the run; review it again before promotion");
    }
    let patch = diff_bytes(worktree, base_revision)?;
    validate_target_paths(source, &review.changed_files)?;
    validate_isolated_links(worktree, &review.changed_files)?;
    if patch.is_empty() {
        return Ok(PromotionOutcome::Applied);
    }

    let check = git_command(
        source,
        &["apply", "--check", "--binary", "--whitespace=nowarn"],
        &[],
        Some(&patch),
    )?;
    if !check.status.success() {
        bail!("promotion preflight failed: {}", command_error(&check));
    }
    let applied = git_command(
        source,
        &["apply", "--binary", "--whitespace=nowarn"],
        &[],
        Some(&patch),
    )?;
    if !applied.status.success() {
        bail!("promotion apply failed: {}", command_error(&applied));
    }

    // `git diff` represents isolated untracked files through intent-to-add.
    // Mirror that representation after applying so the final fingerprint is
    // comparable and retries remain idempotent.
    ensure_intent_to_add(source)?;
    let after = fingerprint_at(source, base_revision)?;
    if after != final_fingerprint {
        let rollback = git_command(
            source,
            &["apply", "--reverse", "--binary", "--whitespace=nowarn"],
            &[],
            Some(&patch),
        )?;
        if !rollback.status.success() {
            bail!(
                "promotion verification failed and rollback also failed: {}",
                command_error(&rollback)
            );
        }
        bail!("promotion verification failed; the source workspace was rolled back");
    }
    Ok(PromotionOutcome::Applied)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromotionOutcome {
    Applied,
    AlreadyApplied,
}

pub(crate) fn discard(source: &Path, worktree: &Path) -> Result<()> {
    let source = dunce::canonicalize(source).context("canonicalize discard source workspace")?;
    let worktree = dunce::canonicalize(worktree).context("canonicalize run worktree")?;
    validate_managed_worktree(&source, &worktree)?;
    let output = git_command(
        &source,
        &["worktree", "remove", "--force"],
        &[worktree.as_os_str()],
        None,
    )?;
    if !output.status.success() {
        bail!("discard isolated run failed: {}", command_error(&output));
    }
    Ok(())
}

pub(crate) fn validate_worktree_path(worktree: &Path) -> Result<()> {
    let worktree = dunce::canonicalize(worktree).context("canonicalize run worktree")?;
    let dot_git = worktree.join(".git");
    let metadata = fs::symlink_metadata(&dot_git).context("read isolated Git metadata")?;
    if metadata.file_type().is_file() {
        let git_dir = fs::read_to_string(&dot_git).context("read isolated worktree metadata")?;
        if !git_dir.contains("worktrees") {
            bail!("isolated run path is not a Git worktree");
        }
    } else if metadata.file_type().is_dir() {
        let private_git =
            dunce::canonicalize(&dot_git).context("canonicalize private isolated Git directory")?;
        if !private_git.starts_with(&worktree) || private_git.join("commondir").exists() {
            bail!("private isolated Git directory is not self-contained");
        }
    } else {
        bail!("isolated run worktree is unavailable");
    }
    Ok(())
}

fn ensure_intent_to_add(worktree: &Path) -> Result<()> {
    let raw = git_command(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        &[],
        None,
    )?;
    if !raw.status.success() {
        bail!(
            "list isolated untracked files failed: {}",
            command_error(&raw)
        );
    }
    let paths = parse_raw_paths(&raw.stdout)?
        .into_iter()
        .filter(|path| {
            !Path::new(path).components().any(|component| {
                component
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".grokptah")
            })
        })
        .map(|path| {
            validate_relative_path(Path::new(&path))?;
            Ok(path)
        })
        .collect::<Result<Vec<_>>>()?;
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![std::ffi::OsStr::new("-N"), std::ffi::OsStr::new("--")];
    let owned: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    args.extend(owned.iter().map(|p| p.as_os_str()));
    let output = git_command(worktree, &["add"], &args, None)?;
    if !output.status.success() {
        bail!(
            "prepare isolated untracked files failed: {}",
            command_error(&output)
        );
    }
    Ok(())
}

fn changed_files(worktree: &Path, base_revision: &str) -> Result<Vec<ChangeRecord>> {
    let output = git_command(
        worktree,
        &["diff", "--name-only", "-z"],
        &[
            std::ffi::OsStr::new(base_revision),
            std::ffi::OsStr::new("--"),
        ],
        None,
    )?;
    if !output.status.success() {
        bail!("list isolated changes failed: {}", command_error(&output));
    }
    let paths = parse_paths(&output.stdout)?;
    if paths.len() > MAX_CHANGED_FILES {
        bail!("isolated run changed too many files");
    }
    Ok(paths
        .into_iter()
        .map(|path| ChangeRecord {
            path,
            summary: "changed in isolated run".into(),
        })
        .collect())
}

fn diff_bytes(worktree: &Path, base_revision: &str) -> Result<Vec<u8>> {
    let output = git_command(
        worktree,
        &["diff", "--binary", "--no-ext-diff", "--no-textconv"],
        &[
            std::ffi::OsStr::new(base_revision),
            std::ffi::OsStr::new("--"),
        ],
        None,
    )?;
    if !output.status.success() {
        bail!("read isolated diff failed: {}", command_error(&output));
    }
    if output.stdout.len() > MAX_PATCH_BYTES {
        bail!("isolated diff exceeds the 32 MiB promotion limit");
    }
    Ok(output.stdout)
}

pub(crate) fn fingerprint_at(root: &Path, base_revision: &str) -> Result<String> {
    let head = git_stdout(root, &["rev-parse", "HEAD"])?;
    let patch = diff_bytes(root, base_revision)?;
    Ok(fingerprint_bytes(&head, &patch))
}

pub(crate) fn fingerprint_bytes(head: &str, patch: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    hasher.update([0]);
    hasher.update(patch);
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PathManifestEntry {
    pub path: String,
    pub before_mode: String,
    pub after_mode: String,
    pub before_blob: String,
    pub after_blob: String,
    pub state: String,
    pub symlink: bool,
    pub untracked: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CapturedWorktreeChanges {
    pub patch: Vec<u8>,
    pub final_fingerprint: String,
    pub manifest: Vec<PathManifestEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceClassification {
    NotApplied,
    AlreadyApplied,
    Poisoned,
}

/// Changed-path identity for one worktree. Unchanged files are not read.
pub(crate) fn capture_worktree_changes(
    worktree: &Path,
    base_revision: &str,
) -> Result<CapturedWorktreeChanges> {
    ensure_intent_to_add(worktree)?;
    let patch = diff_bytes(worktree, base_revision)?;
    let manifest = path_manifest(worktree, base_revision)?;
    validate_isolated_links(
        worktree,
        &manifest
            .iter()
            .map(|entry| ChangeRecord {
                path: entry.path.clone(),
                summary: entry.state.clone(),
            })
            .collect::<Vec<_>>(),
    )?;
    let final_fingerprint = fingerprint_at(worktree, base_revision)?;
    Ok(CapturedWorktreeChanges {
        patch,
        final_fingerprint,
        manifest,
    })
}

pub(crate) fn classify_source(
    source: &Path,
    base_revision: &str,
    source_fingerprint: &str,
    final_fingerprint: &str,
) -> Result<SourceClassification> {
    let current = fingerprint_at(source, base_revision)?;
    if current == final_fingerprint {
        Ok(SourceClassification::AlreadyApplied)
    } else if current == source_fingerprint {
        Ok(SourceClassification::NotApplied)
    } else {
        Ok(SourceClassification::Poisoned)
    }
}

/// Apply one recorded promotion patch with the same preflight, fingerprint,
/// and rollback contract as [`promote`]. A later file failure rolls back
/// earlier files before returning. Fault 3 leaves the first effect in place
/// so a crashed process is classified as poisoned rather than as a no-op.
pub(crate) fn apply_recorded_patch(
    source: &Path,
    base_revision: &str,
    source_fingerprint: &str,
    patch: &[u8],
    final_fingerprint: &str,
    fault: u8,
) -> Result<PromotionOutcome> {
    match classify_source(source, base_revision, source_fingerprint, final_fingerprint)? {
        SourceClassification::AlreadyApplied => return Ok(PromotionOutcome::AlreadyApplied),
        SourceClassification::Poisoned => {
            bail!(
                "source is neither the verified base nor the final candidate; reconciliation is required"
            );
        }
        SourceClassification::NotApplied => {}
    }
    if patch.is_empty() {
        if source_fingerprint == final_fingerprint {
            return Ok(PromotionOutcome::Applied);
        }
        bail!("the candidate change is missing from the promotion patch");
    }
    let parts = split_patch(patch);
    if parts.is_empty() {
        bail!("the promotion patch could not be read");
    }
    for part in &parts {
        let check = git_command(
            source,
            &["apply", "--check", "--binary", "--whitespace=nowarn"],
            &[],
            Some(part),
        )?;
        if !check.status.success() {
            bail!("promotion preflight failed: {}", command_error(&check));
        }
    }
    if fault == 6 {
        let failed = git_command(source, &["apply", "--binary"], &[], Some(b"not a patch\n"))?;
        if failed.status.success() {
            bail!("promotion apply command failed and the source was not changed");
        }
        match classify_source(source, base_revision, source_fingerprint, final_fingerprint)? {
            SourceClassification::NotApplied => {
                bail!("promotion apply command failed and the source was not changed");
            }
            _ => bail!("source effect is partial; reconciliation is required"),
        }
    }
    let mut applied: Vec<Vec<u8>> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if fault == 9 && index + 1 == parts.len() && parts.len() >= 2 {
            let failed = git_command(source, &["apply", "--binary"], &[], Some(b"not a patch\n"))?;
            if failed.status.success() {
                bail!("promotion apply command failed and the source was not changed");
            }
            if !rollback_parts(source, &applied) {
                bail!("promotion verification failed and rollback also failed");
            }
            match classify_source(source, base_revision, source_fingerprint, final_fingerprint)? {
                SourceClassification::NotApplied => {
                    bail!("promotion apply command failed and the source was rolled back");
                }
                _ => bail!("promotion verification failed and rollback also failed"),
            }
        }
        let applied_now = git_command(
            source,
            &["apply", "--binary", "--whitespace=nowarn"],
            &[],
            Some(part),
        )?;
        if !applied_now.status.success() {
            if !rollback_parts(source, &applied) {
                bail!("promotion verification failed and rollback also failed");
            }
            match classify_source(source, base_revision, source_fingerprint, final_fingerprint)? {
                SourceClassification::NotApplied => {
                    bail!("promotion apply command failed and the source was rolled back");
                }
                _ => bail!("promotion verification failed and rollback also failed"),
            }
        }
        applied.push(part.clone());
        if fault == 3 && index == 0 && parts.len() >= 2 {
            bail!("source effect is partial; reconciliation is required");
        }
        if fault == 7 && index == 0 {
            if let Some(path) = first_patch_path(part) {
                let _ = fs::write(source.join(&path), b"rollback-poison\n");
            }
            if rollback_parts(source, &applied) {
                bail!("rollback fault injection did not stick");
            }
            bail!("promotion verification failed and rollback also failed");
        }
    }
    ensure_intent_to_add(source)?;
    let after = fingerprint_at(source, base_revision)?;
    if after != final_fingerprint {
        if !rollback_parts(source, &applied) {
            bail!("promotion verification failed and rollback also failed");
        }
        bail!("promotion verification failed; the source workspace was rolled back");
    }
    Ok(PromotionOutcome::Applied)
}

pub(crate) fn manifest_digest(base_revision: &str, entries: &[PathManifestEntry]) -> String {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-changed-path-v1\0");
    hasher.update(base_revision.as_bytes());
    hasher.update([0]);
    for entry in &ordered {
        hasher.update(entry.path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.before_mode.as_bytes());
        hasher.update([0]);
        hasher.update(entry.after_mode.as_bytes());
        hasher.update([0]);
        hasher.update(entry.before_blob.as_bytes());
        hasher.update([0]);
        hasher.update(entry.after_blob.as_bytes());
        hasher.update([0]);
        hasher.update(entry.state.as_bytes());
        hasher.update([0]);
        hasher.update([u8::from(entry.symlink), u8::from(entry.untracked), b'\n']);
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn materialize_manifest(
    worktree: &Path,
    dest: &Path,
    entries: &[PathManifestEntry],
) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest).context("replace candidate snapshot")?;
    }
    fs::create_dir_all(dest).context("create candidate snapshot")?;
    for entry in entries {
        if entry.state == "delete" {
            continue;
        }
        let from = worktree.join(&entry.path);
        let to = dest.join(&entry.path);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).context("create candidate snapshot directory")?;
        }
        let meta = fs::symlink_metadata(&from)
            .with_context(|| format!("read candidate {}", entry.path))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&from).context("read candidate symlink")?;
            if target.is_absolute() {
                bail!("promotion refuses absolute symlinks: {}", entry.path);
            }
            let resolved = dunce::canonicalize(from.parent().unwrap_or(worktree).join(&target))
                .with_context(|| format!("resolve candidate symlink {}", entry.path))?;
            let root = dunce::canonicalize(worktree).context("canonicalize candidate worktree")?;
            if !resolved.starts_with(&root) {
                bail!("promotion refuses symlink escape: {}", entry.path);
            }
            std::os::unix::fs::symlink(&target, &to).context("retain candidate symlink")?;
            continue;
        }
        if !meta.is_file() {
            bail!("the candidate contains a non-regular source file");
        }
        fs::copy(&from, &to).with_context(|| format!("retain {}", entry.path))?;
        if let Ok(mode) = u32::from_str_radix(&entry.after_mode, 8) {
            fs::set_permissions(&to, fs::Permissions::from_mode(mode & 0o777))
                .with_context(|| format!("retain mode {}", entry.path))?;
        }
    }
    Ok(())
}

fn path_manifest(worktree: &Path, base_revision: &str) -> Result<Vec<PathManifestEntry>> {
    let output = git_command(
        worktree,
        &["diff", "--raw", "--no-renames", "-z", "--no-ext-diff"],
        &[
            std::ffi::OsStr::new(base_revision),
            std::ffi::OsStr::new("--"),
        ],
        None,
    )?;
    if !output.status.success() {
        bail!("list isolated changes failed: {}", command_error(&output));
    }
    let mut entries = Vec::new();
    let fields: Vec<&[u8]> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect();
    let mut index = 0;
    while index + 1 < fields.len() {
        let header = std::str::from_utf8(fields[index]).context("git returned a non-UTF-8 diff")?;
        let path =
            std::str::from_utf8(fields[index + 1]).context("git returned a non-UTF-8 path")?;
        index += 2;
        validate_relative_path(Path::new(path))?;
        // `git diff --raw -z` emits ":oldmode newmode oldsha newsha status\0path\0".
        let header_parts: Vec<&str> = header.split_whitespace().collect();
        if header_parts.len() < 5 {
            bail!("isolated change header is incomplete");
        }
        let before_mode = header_parts[0].trim_start_matches(':').to_string();
        let after_mode = header_parts[1].to_string();
        let before_blob = header_parts[2].to_string();
        let after_blob = header_parts[3].to_string();
        let status = header_parts[4];
        let state = if status.starts_with('A') {
            "add"
        } else if status.starts_with('D') {
            "delete"
        } else if before_mode != after_mode
            && (before_blob == after_blob || after_blob == "0000000")
        {
            "mode"
        } else {
            "modify"
        };
        let tracked = git_command(
            worktree,
            &["ls-files", "--error-unmatch", "--"],
            &[std::ffi::OsStr::new(path)],
            None,
        )
        .map(|output| output.status.success())
        .unwrap_or(false);
        entries.push(PathManifestEntry {
            path: path.to_string(),
            before_mode: before_mode.clone(),
            after_mode: after_mode.clone(),
            before_blob,
            after_blob,
            state: state.to_string(),
            symlink: before_mode == "120000" || after_mode == "120000",
            untracked: status.starts_with('A') && !tracked,
        });
    }
    if entries.len() > MAX_CHANGED_FILES {
        bail!(
            "the candidate changes more files than the host changed-path bound of {MAX_CHANGED_FILES}"
        );
    }
    Ok(entries)
}

fn split_patch(patch: &[u8]) -> Vec<Vec<u8>> {
    if patch.is_empty() {
        return Vec::new();
    }
    let marker = b"\ndiff --git ";
    let mut starts = vec![0usize];
    let mut search = 0usize;
    while search + marker.len() <= patch.len() {
        if let Some(found) = patch[search..]
            .windows(marker.len())
            .position(|window| window == marker)
        {
            let at = search + found + 1;
            starts.push(at);
            search = at + 1;
        } else {
            break;
        }
    }
    let mut parts = Vec::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(patch.len());
        if end > start {
            parts.push(patch[start..end].to_vec());
        }
    }
    parts
}

fn rollback_parts(source: &Path, parts: &[Vec<u8>]) -> bool {
    for part in parts.iter().rev() {
        let reversed = git_command(
            source,
            &["apply", "--reverse", "--binary", "--whitespace=nowarn"],
            &[],
            Some(part),
        );
        if reversed
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            continue;
        }
        return false;
    }
    true
}

fn first_patch_path(part: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(part);
    let line = text.lines().find(|line| line.starts_with("diff --git "))?;
    let mut pieces = line.split_whitespace();
    let _ = pieces.next();
    let _ = pieces.next();
    let path = pieces.next()?;
    let relative = path.strip_prefix("a/").unwrap_or(path);
    if relative.is_empty() || relative.contains('\0') {
        None
    } else {
        Some(relative.to_string())
    }
}

fn parse_paths(bytes: &[u8]) -> Result<Vec<String>> {
    parse_raw_paths(bytes)?
        .into_iter()
        .map(|path| {
            validate_relative_path(Path::new(&path))?;
            Ok(path)
        })
        .collect()
}

fn parse_raw_paths(bytes: &[u8]) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for raw in bytes.split(|byte| *byte == 0).filter(|raw| !raw.is_empty()) {
        let path = std::str::from_utf8(raw).context("Git returned a non-UTF-8 path")?;
        paths.push(path.to_string());
    }
    Ok(paths)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("isolated change path is not relative");
    }
    let components: Vec<_> = path.components().collect();
    if components.iter().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!(
            "isolated change path escapes the workspace: {}",
            path.display()
        );
    }
    if components.iter().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
            || component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".grokptah")
    }) {
        bail!(
            "isolated change path targets protected metadata: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_target_paths(source: &Path, changed: &[ChangeRecord]) -> Result<()> {
    let source = dunce::canonicalize(source).context("canonicalize promotion source")?;
    for change in changed {
        let path = Path::new(&change.path);
        validate_relative_path(path)?;
        let mut current = source.clone();
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            current.push(component.as_os_str());
            if components.peek().is_some()
                && fs::symlink_metadata(&current)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false)
            {
                bail!("promotion path traverses a symlink: {}", path.display());
            }
        }
        if let Some(parent) = current.parent() {
            if parent.exists() && !dunce::canonicalize(parent)?.starts_with(&source) {
                bail!(
                    "promotion path escapes the source workspace: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn validate_isolated_links(worktree: &Path, changed: &[ChangeRecord]) -> Result<()> {
    let root = dunce::canonicalize(worktree).context("canonicalize isolated worktree")?;
    for change in changed {
        let path = root.join(&change.path);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let target = fs::read_link(&path).context("read isolated symlink")?;
        if target.is_absolute() {
            bail!("promotion refuses absolute symlinks: {}", change.path);
        }
        let resolved = dunce::canonicalize(path.parent().unwrap_or(&root).join(target))
            .with_context(|| format!("resolve isolated symlink {}", change.path))?;
        if !resolved.starts_with(&root) {
            bail!("promotion refuses symlink escape: {}", change.path);
        }
    }
    Ok(())
}

fn bounded_text(bytes: &[u8], limit: usize) -> (String, bool) {
    if bytes.len() <= limit {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut text = String::from_utf8_lossy(&bytes[..limit]).into_owned();
    text.push_str("\n\n[diff truncated]");
    (text, true)
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_command(root, args, &[], None)?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), command_error(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_stdout_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_command(root, args, &[], None)?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), command_error(&output));
    }
    Ok(output.stdout)
}

fn git_command(
    root: &Path,
    args: &[&str],
    extra: &[&std::ffi::OsStr],
    input: Option<&[u8]>,
) -> Result<Output> {
    let mut command = Command::new("git");
    command.args(args).args(extra).current_dir(root);
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().context("start Git operation")?;
    if let Some(bytes) = input {
        child
            .stdin
            .take()
            .context("open Git operation stdin")?
            .write_all(bytes)
            .context("write Git operation input")?;
    }
    child.wait_with_output().context("wait for Git operation")
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "GrokPtah tests")
            .env("GIT_AUTHOR_EMAIL", "tests@grokptah.invalid")
            .env("GIT_COMMITTER_NAME", "GrokPtah tests")
            .env("GIT_COMMITTER_EMAIL", "tests@grokptah.invalid")
            .output()
            .expect("start git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("README.md"), "base\n").unwrap();
        git(dir.path(), &["add", "README.md"]);
        git(dir.path(), &["commit", "-qm", "base"]);
        dir
    }

    #[test]
    fn prepare_requires_clean_git_workspace() {
        let dir = repository();
        fs::write(dir.path().join("README.md"), "dirty\n").unwrap();
        let error = prepare(dir.path(), "dirty").unwrap_err().to_string();
        assert!(error.contains("clean Git workspace"), "{error}");
    }

    #[test]
    fn isolated_diff_can_be_reviewed_promoted_and_retried() {
        let dir = repository();
        let prepared = prepare(dir.path(), "roundtrip").unwrap();
        fs::write(prepared.cwd.join("README.md"), "changed\n").unwrap();
        fs::write(prepared.cwd.join("new.txt"), "new\n").unwrap();

        let snapshot = snapshot(&prepared.cwd, &prepared.base_revision).unwrap();
        assert_eq!(snapshot.changed_files.len(), 2);
        let review = review(&prepared.cwd, &prepared.base_revision).unwrap();
        assert!(review.diff.contains("changed"));
        assert_eq!(review.fingerprint, snapshot.fingerprint);

        let outcome = promote(
            dir.path(),
            &prepared.cwd,
            &prepared.base_revision,
            &prepared.source_fingerprint,
            &snapshot.fingerprint,
        )
        .unwrap();
        assert_eq!(outcome, PromotionOutcome::Applied);
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "new\n"
        );

        let retry = promote(
            dir.path(),
            &prepared.cwd,
            &prepared.base_revision,
            &prepared.source_fingerprint,
            &snapshot.fingerprint,
        )
        .unwrap();
        assert_eq!(retry, PromotionOutcome::AlreadyApplied);
    }

    #[test]
    fn source_change_blocks_promotion() {
        let dir = repository();
        let prepared = prepare(dir.path(), "conflict").unwrap();
        fs::write(prepared.cwd.join("README.md"), "isolated\n").unwrap();
        let snapshot = snapshot(&prepared.cwd, &prepared.base_revision).unwrap();
        fs::write(dir.path().join("README.md"), "source changed\n").unwrap();

        let error = promote(
            dir.path(),
            &prepared.cwd,
            &prepared.base_revision,
            &prepared.source_fingerprint,
            &snapshot.fingerprint,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("source workspace changed"), "{error}");
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "source changed\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_before_promotion() {
        use std::os::unix::fs::symlink;

        let dir = repository();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let prepared = prepare(dir.path(), "symlink").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            prepared.cwd.join("leak.txt"),
        )
        .unwrap();
        let snapshot = snapshot(&prepared.cwd, &prepared.base_revision).unwrap();
        let error = promote(
            dir.path(),
            &prepared.cwd,
            &prepared.base_revision,
            &prepared.source_fingerprint,
            &snapshot.fingerprint,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("symlink"), "{error}");
    }

    #[test]
    fn discard_removes_only_managed_worktree() {
        let dir = repository();
        let prepared = prepare(dir.path(), "discard").unwrap();
        assert!(prepared.cwd.exists());
        discard(dir.path(), &prepared.cwd).unwrap();
        assert!(!prepared.cwd.exists());
        assert!(dir.path().join("README.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_root_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let dir = repository();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".grokptah/worktrees")).unwrap();
        symlink(outside.path(), dir.path().join(".grokptah/worktrees/runs")).unwrap();
        let error = validate_managed_worktree(dir.path(), outside.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside the source workspace"), "{error}");
    }

    #[test]
    fn mode_only_change_applies_exactly() {
        let dir = repository();
        let base = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let base = String::from_utf8(base.stdout).unwrap().trim().to_string();
        let source_fingerprint = fingerprint_at(dir.path(), &base).unwrap();
        let readme = dir.path().join("README.md");
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o755)).unwrap();
        let captured = capture_worktree_changes(dir.path(), &base).unwrap();
        assert!(
            captured
                .manifest
                .iter()
                .any(|entry| entry.before_mode != entry.after_mode),
            "{:?}",
            captured.manifest
        );
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            fingerprint_at(dir.path(), &base).unwrap(),
            source_fingerprint
        );
        apply_recorded_patch(
            dir.path(),
            &base,
            &source_fingerprint,
            &captured.patch,
            &captured.final_fingerprint,
            0,
        )
        .unwrap();
        let mode = fs::symlink_metadata(&readme).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(fs::read(&readme).unwrap(), b"base\n");
    }

    #[test]
    fn changed_path_identity_does_not_hash_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        for index in 0..300 {
            fs::write(
                dir.path().join(format!("src/file-{index}.rs")),
                format!("fn value_{index}() {{}}\n"),
            )
            .unwrap();
        }
        git(dir.path(), &["init", "-b", "main"]);
        git(
            dir.path(),
            &["config", "user.email", "tests@grokptah.invalid"],
        );
        git(dir.path(), &["config", "user.name", "GrokPtah tests"]);
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "base"]);
        let base = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let base = String::from_utf8(base.stdout).unwrap().trim().to_string();
        fs::write(
            dir.path().join("src/file-0.rs"),
            "fn value_0() { changed }\n",
        )
        .unwrap();
        let captured = capture_worktree_changes(dir.path(), &base).unwrap();
        assert_eq!(captured.manifest.len(), 1);
        assert_eq!(captured.manifest[0].path, "src/file-0.rs");
        assert!(captured.patch.len() < 32 * 1024 * 1024);
    }
}
