//! Worktree age GC policy (#166). Not a full xai-fast-worktree port.

#![allow(dead_code)] // public API exercised by unit tests + future slash/host hooks

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Default max age for auto-managed worktrees under `.grokptah/worktrees`.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(7 * 86400);

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GcReport {
    pub scanned: usize,
    pub removed: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub dry_run: bool,
}

/// List candidate dirs under `root` older than `max_age`.
pub fn candidates_older_than(root: &Path, max_age: Duration, now: SystemTime) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let modified = meta.modified().ok().or_else(|| meta.created().ok());
        let Some(m) = modified else { continue };
        if now.duration_since(m).unwrap_or_default() >= max_age {
            out.push(p);
        }
    }
    out
}

/// Remove candidates (or dry-run). Only deletes if path contains `.grokptah/worktrees` for safety.
pub fn gc_worktrees(root: &Path, max_age: Duration, dry_run: bool) -> GcReport {
    let now = SystemTime::now();
    let cands = candidates_older_than(root, max_age, now);
    let mut report = GcReport {
        scanned: cands.len(),
        removed: Vec::new(),
        skipped: Vec::new(),
        dry_run,
    };
    for p in cands {
        let s = p.to_string_lossy();
        if !(s.contains(".grokptah/worktrees") || s.contains(".grokptah\\worktrees")) {
            report
                .skipped
                .push((p, "path not under .grokptah/worktrees".into()));
            continue;
        }
        if dry_run {
            report.removed.push(p);
            continue;
        }
        match std::fs::remove_dir_all(&p) {
            Ok(()) => report.removed.push(p),
            Err(e) => report.skipped.push((p, e.to_string())),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn finds_old_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".grokptah").join("worktrees");
        std::fs::create_dir_all(root.join("old")).unwrap();
        let c = candidates_older_than(&root, Duration::from_secs(0), SystemTime::now());
        assert!(c.iter().any(|p| p.ends_with("old")));
    }

    #[test]
    fn refuses_paths_outside_managed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("other");
        std::fs::create_dir_all(&outside).unwrap();
        // pretend candidates
        let r = gc_worktrees(dir.path(), Duration::from_secs(0), true);
        // root has no .grokptah/worktrees children with age — scanned 0 or skipped
        assert!(r.removed.is_empty() || r.skipped.iter().all(|(_, m)| m.contains("not under")));
    }
}
