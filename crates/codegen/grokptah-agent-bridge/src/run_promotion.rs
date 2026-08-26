//! Strict Git-backed isolation and promotion for desktop Build runs.
//!
//! This module deliberately has a narrower contract than the subagent
//! isolation helper. Promotable runs start from a clean Git workspace, keep a
//! detached worktree under the project, and promote only a reviewed diff when
//! the source fingerprint is unchanged.

use std::fs;
use std::io::Write;
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

/// Whether a workspace can back an isolated, promotable run right now.
///
/// A closed vocabulary rather than a boolean: "not a Git repository" and
/// "Git repository with uncommitted work" call for different operator
/// actions, and neither is the same as "the Git tooling is unavailable".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationReadiness {
    /// A Git workspace at a commit with no uncommitted or untracked changes.
    /// Isolation can be prepared and later promoted through review.
    CleanGit,
    /// A Git workspace with uncommitted or untracked changes. Isolating now
    /// would silently leave that work outside the run.
    DirtyGit,
    /// Not a Git workspace, so there is no base revision to isolate from.
    NotGit,
    /// Git could not be consulted at all (missing binary, unreadable path).
    Unavailable,
}

impl IsolationReadiness {
    /// The exact wire value used for logging and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CleanGit => "clean_git",
            Self::DirtyGit => "dirty_git",
            Self::NotGit => "not_git",
            Self::Unavailable => "unavailable",
        }
    }

    /// Whether a *new* session may default to an isolated worktree.
    ///
    /// Only a clean Git workspace qualifies: [`prepare`] refuses everything
    /// else, so defaulting to isolation there would produce a session whose
    /// first turn always fails.
    pub const fn permits_default_isolation(self) -> bool {
        matches!(self, Self::CleanGit)
    }
}

/// Probe whether a workspace can back an isolated run, without touching it.
///
/// Read-only by construction: it runs `rev-parse` and `status`, and creates no
/// directory, worktree, or file. [`prepare`] performs the same two checks
/// before it mutates anything, so a `CleanGit` answer here and a successful
/// `prepare` cannot disagree about what "clean" means.
pub fn isolation_readiness(project: &Path) -> IsolationReadiness {
    let Ok(source) = dunce::canonicalize(project) else {
        return IsolationReadiness::Unavailable;
    };
    if !source.is_dir() {
        return IsolationReadiness::Unavailable;
    }
    match git_command(&source, &["rev-parse", "--verify", "HEAD"], &[], None) {
        // A repository with no commit yet has no base revision to isolate
        // from, which `prepare` also refuses.
        Ok(output) if !output.status.success() => return IsolationReadiness::NotGit,
        Ok(_) => {}
        Err(_) => return IsolationReadiness::Unavailable,
    }
    match git_command(
        &source,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        &[],
        None,
    ) {
        Ok(output) if !output.status.success() => IsolationReadiness::NotGit,
        Ok(output) if output.stdout.is_empty() => IsolationReadiness::CleanGit,
        Ok(_) => IsolationReadiness::DirtyGit,
        Err(_) => IsolationReadiness::Unavailable,
    }
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
    if !worktree.join(".git").is_file() {
        bail!("isolated run worktree is unavailable");
    }
    let git_dir =
        fs::read_to_string(worktree.join(".git")).context("read isolated worktree metadata")?;
    if !git_dir.contains("worktrees") {
        bail!("isolated run path is not a Git worktree");
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

fn fingerprint_at(root: &Path, base_revision: &str) -> Result<String> {
    let head = git_stdout(root, &["rev-parse", "HEAD"])?;
    let patch = diff_bytes(root, base_revision)?;
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    hasher.update([0]);
    hasher.update(patch);
    Ok(format!("{:x}", hasher.finalize()))
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

    /// The probe and [`prepare`] must agree about what "clean" means: a
    /// session that defaults to isolation on a workspace `prepare` will refuse
    /// would fail on its very first turn.
    #[test]
    fn the_isolation_probe_agrees_with_what_prepare_will_accept() {
        let clean = repository();
        assert_eq!(
            isolation_readiness(clean.path()),
            IsolationReadiness::CleanGit
        );
        assert!(isolation_readiness(clean.path()).permits_default_isolation());
        prepare(clean.path(), "probe-agrees").expect("a clean workspace prepares");

        // An untracked file is uncommitted work that isolation would leave
        // behind, so both the probe and `prepare` refuse it.
        let dirty = repository();
        fs::write(dirty.path().join("scratch.txt"), "uncommitted\n").unwrap();
        assert_eq!(
            isolation_readiness(dirty.path()),
            IsolationReadiness::DirtyGit
        );
        assert!(!isolation_readiness(dirty.path()).permits_default_isolation());
        assert!(prepare(dirty.path(), "probe-agrees").is_err());

        // A tracked-but-modified file is the same answer.
        let modified = repository();
        fs::write(modified.path().join("README.md"), "changed\n").unwrap();
        assert_eq!(
            isolation_readiness(modified.path()),
            IsolationReadiness::DirtyGit
        );
        assert!(prepare(modified.path(), "probe-agrees").is_err());
    }

    #[test]
    fn a_workspace_without_a_base_revision_is_never_isolated_by_default() {
        // Not a repository at all.
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            isolation_readiness(plain.path()),
            IsolationReadiness::NotGit
        );
        assert!(!isolation_readiness(plain.path()).permits_default_isolation());
        assert!(prepare(plain.path(), "no-base").is_err());

        // A repository with no commit has no HEAD to isolate from.
        let empty = tempfile::tempdir().unwrap();
        git(empty.path(), &["init", "-q"]);
        assert_eq!(
            isolation_readiness(empty.path()),
            IsolationReadiness::NotGit
        );
        assert!(prepare(empty.path(), "no-base").is_err());

        // A path that does not exist is a tooling answer, not a Git answer.
        let missing = plain.path().join("does-not-exist");
        assert_eq!(
            isolation_readiness(&missing),
            IsolationReadiness::Unavailable
        );
        assert!(!isolation_readiness(&missing).permits_default_isolation());
    }

    #[test]
    fn the_probe_creates_nothing() {
        let clean = repository();
        let listing = |root: &Path| {
            let mut names: Vec<_> = fs::read_dir(root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = listing(clean.path());
        assert_eq!(
            isolation_readiness(clean.path()),
            IsolationReadiness::CleanGit
        );
        assert_eq!(
            listing(clean.path()),
            before,
            "the probe wrote to the workspace"
        );
        // Specifically: no managed worktree root appeared.
        assert!(!clean.path().join(".grokptah").exists());
        // And the probe left the workspace clean, so it is still preparable.
        assert_eq!(
            isolation_readiness(clean.path()),
            IsolationReadiness::CleanGit
        );
    }

    #[test]
    fn only_a_clean_git_workspace_permits_default_isolation() {
        for readiness in [
            IsolationReadiness::CleanGit,
            IsolationReadiness::DirtyGit,
            IsolationReadiness::NotGit,
            IsolationReadiness::Unavailable,
        ] {
            assert_eq!(
                readiness.permits_default_isolation(),
                readiness == IsolationReadiness::CleanGit,
                "{readiness:?}"
            );
            assert!(!readiness.as_str().is_empty());
        }
    }
}
