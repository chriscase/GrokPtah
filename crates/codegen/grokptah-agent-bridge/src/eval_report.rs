//! Evidence helpers for disposable agent evaluations.
//!
//! Reports deliberately contain relative paths, sizes, and hashes rather than
//! file contents or absolute workspace paths. This makes failed-run artifacts
//! useful without turning them into a data-exfiltration channel.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

const IGNORED_DIRECTORY_NAMES: &[&str] = &[".git", "node_modules", "target"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceSnapshot {
    files: BTreeMap<String, FileFingerprint>,
    symlinks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub change: String,
    pub before_bytes: Option<u64>,
    pub after_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct WorkspaceChangeSummary {
    pub added: u32,
    pub modified: u32,
    pub removed: u32,
    pub changed_files: Vec<ChangedFile>,
    pub symlinks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct HandoffEvidence {
    pub present: bool,
    pub chars: usize,
    pub sha256: Option<String>,
    pub mentions_changes: bool,
    pub mentions_tests: bool,
}

impl WorkspaceChangeSummary {
    pub fn has_changes(&self) -> bool {
        self.added > 0 || self.modified > 0 || self.removed > 0
    }

    pub fn has_safety_findings(&self) -> bool {
        !self.symlinks.is_empty()
    }
}

pub fn snapshot_workspace(root: &Path) -> Result<WorkspaceSnapshot, String> {
    if !root.is_dir() {
        return Err(format!("workspace is not a directory: {}", root.display()));
    }

    let mut snapshot = WorkspaceSnapshot::default();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !IGNORED_DIRECTORY_NAMES.contains(&entry.file_name().to_string_lossy().as_ref())
        })
    {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.depth() == 0 {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if entry.file_type().is_symlink() {
            snapshot.symlinks.push(relative);
        } else if entry.file_type().is_file() {
            let body = fs::read(entry.path()).map_err(|error| error.to_string())?;
            let digest = Sha256::digest(&body);
            snapshot.files.insert(
                relative,
                FileFingerprint {
                    bytes: body.len() as u64,
                    sha256: format!("{digest:x}"),
                },
            );
        }
    }
    snapshot.symlinks.sort();
    Ok(snapshot)
}

pub fn path_is_within_workspace(root: &Path, candidate: &str) -> bool {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let candidate = PathBuf::from(candidate);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    };
    dunce::canonicalize(&candidate)
        .unwrap_or(candidate)
        .starts_with(root)
}

pub fn report_path(root: &Path, candidate: &str) -> String {
    let path = Path::new(candidate);
    if !path.is_absolute() {
        return candidate.replace('\\', "/");
    }
    if !path_is_within_workspace(root, candidate) {
        return "<outside-workspace>".into();
    }
    dunce::canonicalize(path)
        .ok()
        .and_then(|path| path.strip_prefix(root).ok().map(|path| path.to_path_buf()))
        .unwrap_or_else(|| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn summarize_handoff(text: Option<&str>) -> HandoffEvidence {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return HandoffEvidence::default();
    };
    let lower = text.to_ascii_lowercase();
    let digest = Sha256::digest(text.as_bytes());
    HandoffEvidence {
        present: true,
        chars: text.chars().count(),
        sha256: Some(format!("{digest:x}")),
        mentions_changes: ["changed", "implemented", "modified", "created"]
            .iter()
            .any(|needle| lower.contains(needle)),
        mentions_tests: lower.contains("test") || lower.contains("verif"),
    }
}

pub fn diff_workspace(
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
) -> WorkspaceChangeSummary {
    let mut changed_files = Vec::new();
    let mut added = 0;
    let mut modified = 0;
    let mut removed = 0;

    for (path, current) in &after.files {
        match before.files.get(path) {
            None => {
                added += 1;
                changed_files.push(ChangedFile {
                    path: path.clone(),
                    change: "added".into(),
                    before_bytes: None,
                    after_bytes: Some(current.bytes),
                });
            }
            Some(previous) if previous != current => {
                modified += 1;
                changed_files.push(ChangedFile {
                    path: path.clone(),
                    change: "modified".into(),
                    before_bytes: Some(previous.bytes),
                    after_bytes: Some(current.bytes),
                });
            }
            Some(_) => {}
        }
    }
    for (path, previous) in &before.files {
        if !after.files.contains_key(path) {
            removed += 1;
            changed_files.push(ChangedFile {
                path: path.clone(),
                change: "removed".into(),
                before_bytes: Some(previous.bytes),
                after_bytes: None,
            });
        }
    }

    changed_files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut symlinks = before.symlinks.clone();
    symlinks.extend(after.symlinks.iter().cloned());
    symlinks.sort();
    symlinks.dedup();

    WorkspaceChangeSummary {
        added,
        modified,
        removed,
        changed_files,
        symlinks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reports_added_modified_removed_and_ignores_build_output() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("keep.txt"), "before").unwrap();
        fs::write(root.path().join("removed.txt"), "old").unwrap();
        fs::create_dir_all(root.path().join("target/debug")).unwrap();
        fs::write(root.path().join("target/debug/generated"), "ignored").unwrap();
        let before = snapshot_workspace(root.path()).unwrap();

        fs::remove_file(root.path().join("keep.txt")).unwrap();
        fs::write(root.path().join("keep.txt"), "after").unwrap();
        fs::write(root.path().join("added.txt"), "new").unwrap();
        fs::remove_file(root.path().join("removed.txt")).unwrap();
        let after = snapshot_workspace(root.path()).unwrap();
        let delta = diff_workspace(&before, &after);

        assert_eq!(delta.added, 1);
        assert_eq!(delta.modified, 1);
        assert_eq!(delta.removed, 1);
        assert!(delta
            .changed_files
            .iter()
            .any(|file| file.path == "added.txt"));
        assert!(!delta
            .changed_files
            .iter()
            .any(|file| file.path.starts_with("target/")));
    }

    #[test]
    fn detects_modified_and_removed_files_without_contents() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("same.txt"), "same").unwrap();
        fs::write(root.path().join("change.txt"), "old").unwrap();
        fs::write(root.path().join("gone.txt"), "gone").unwrap();
        let before = snapshot_workspace(root.path()).unwrap();

        fs::write(root.path().join("change.txt"), "new content").unwrap();
        fs::remove_file(root.path().join("gone.txt")).unwrap();
        let after = snapshot_workspace(root.path()).unwrap();
        let delta = diff_workspace(&before, &after);

        assert_eq!(delta.modified, 1);
        assert_eq!(delta.removed, 1);
        assert_eq!(delta.added, 0);
        assert_eq!(delta.changed_files.len(), 2);
        assert!(delta
            .changed_files
            .iter()
            .all(|file| file.path != "same.txt"));
    }

    #[test]
    fn handoff_summary_is_redacted_but_measurable() {
        let evidence = summarize_handoff(Some("Implemented the fix. cargo test passed."));

        assert!(evidence.present);
        assert_eq!(evidence.chars, 39);
        assert!(evidence.sha256.is_some());
        assert!(evidence.mentions_changes);
        assert!(evidence.mentions_tests);
    }

    #[test]
    fn workspace_path_checks_keep_reports_relative_and_redact_escapes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub fn ok() {}\n").unwrap();

        assert!(path_is_within_workspace(root.path(), "src/lib.rs"));
        assert!(!path_is_within_workspace(root.path(), "/tmp/outside.rs"));
        assert_eq!(report_path(root.path(), "src\\lib.rs"), "src/lib.rs");
        assert_eq!(
            report_path(root.path(), "/tmp/outside.rs"),
            "<outside-workspace>"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_symlinks_as_safety_findings() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("inside.txt"), "inside").unwrap();
        symlink("inside.txt", root.path().join("link.txt")).unwrap();
        let snapshot = snapshot_workspace(root.path()).unwrap();

        assert_eq!(snapshot.symlinks, vec!["link.txt"]);
        assert!(diff_workspace(&snapshot, &snapshot).has_safety_findings());
    }
}
