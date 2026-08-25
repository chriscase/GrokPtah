//! Approved inspection roots and fail-closed path containment.
//!
//! Containment is lexical *and* physical. A request is normalised without
//! ever consulting the filesystem (so `..` can never be neutralised by a
//! link), then every component from the root down to the file is stat'd with
//! `symlink_metadata` and refused if it is a link. Only after both passes
//! does the reader open anything.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::error::SourceViewError;

/// Whether a root is the user's shared workspace or an isolated run worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// The workspace the operator explicitly opened.
    Workspace,
    /// A managed worktree owned by one isolated run.
    IsolatedWorktree,
}

impl RootKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Workspace => "ws",
            Self::IsolatedWorktree => "wt",
        }
    }
}

/// An approved inspection boundary with an exact, canonical identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRoot {
    /// Stable identity derived from kind plus canonical path. Two roots that
    /// resolve to the same directory always share an id, so the view layer
    /// can prove which boundary a document came from.
    pub id: String,
    pub kind: RootKind,
    /// Human label for the identity strip (never a substitute for `path`).
    pub label: String,
    /// Canonical absolute path. This is the exact boundary, shown verbatim.
    pub path: PathBuf,
    /// Owning run, for isolated worktrees.
    pub run_id: Option<String>,
}

impl SourceRoot {
    /// Approve a workspace directory. The path is canonicalised here so the
    /// identity cannot drift with the caller's spelling of it.
    pub fn workspace(
        path: impl AsRef<Path>,
        label: impl Into<String>,
    ) -> Result<Self, SourceViewError> {
        Self::approve(RootKind::Workspace, path, label, None)
    }

    /// Approve an isolated run worktree.
    pub fn isolated_worktree(
        path: impl AsRef<Path>,
        label: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Result<Self, SourceViewError> {
        Self::approve(RootKind::IsolatedWorktree, path, label, Some(run_id.into()))
    }

    fn approve(
        kind: RootKind,
        path: impl AsRef<Path>,
        label: impl Into<String>,
        run_id: Option<String>,
    ) -> Result<Self, SourceViewError> {
        let raw = path.as_ref();
        let canonical = dunce::canonicalize(raw).map_err(|_| SourceViewError::RootUnavailable {
            at: raw.display().to_string(),
        })?;
        if !canonical.is_dir() {
            return Err(SourceViewError::RootUnavailable {
                at: canonical.display().to_string(),
            });
        }
        let id = format!(
            "{}-{:016x}",
            kind.prefix(),
            fnv1a64(canonical.to_string_lossy().as_bytes())
        );
        Ok(Self {
            id,
            kind,
            label: label.into(),
            path: canonical,
            run_id,
        })
    }
}

/// A request that passed both containment passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub root: SourceRoot,
    /// Root-relative path, always forward-slash separated for display.
    pub relative_path: String,
    /// Absolute path proven to sit inside `root.path` with no links crossed.
    pub absolute_path: PathBuf,
}

/// The set of roots the operator has approved for read-only inspection.
#[derive(Debug, Clone, Default)]
pub struct SourceRootRegistry {
    roots: BTreeMap<String, SourceRoot>,
}

impl SourceRootRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Approve a root. Re-approving the same identity replaces its label and
    /// run association without creating a second boundary.
    pub fn approve(&mut self, root: SourceRoot) {
        self.roots.insert(root.id.clone(), root);
    }

    pub fn get(&self, root_id: &str) -> Option<&SourceRoot> {
        self.roots.get(root_id)
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Approved roots in stable id order.
    pub fn roots(&self) -> Vec<SourceRoot> {
        self.roots.values().cloned().collect()
    }

    /// Resolve `requested` inside the approved root `root_id`, refusing
    /// anything that escapes the boundary or crosses a symlink.
    pub fn resolve(
        &self,
        root_id: &str,
        requested: &str,
    ) -> Result<ResolvedSource, SourceViewError> {
        if self.roots.is_empty() {
            return Err(SourceViewError::NoApprovedRoot);
        }
        let root = self
            .roots
            .get(root_id)
            .ok_or_else(|| SourceViewError::UnknownRoot {
                root_id: root_id.to_string(),
            })?;
        resolve_in_root(root, requested)
    }
}

/// Resolve one request against a single approved root.
pub fn resolve_in_root(
    root: &SourceRoot,
    requested: &str,
) -> Result<ResolvedSource, SourceViewError> {
    let relative = normalize_relative(&root.path, requested)?;
    let absolute = root.path.join(&relative);

    // Physical pass: every component from the root down must be a real
    // directory entry, never a link. Checking the ancestors (not just the
    // leaf) is what stops `approved/link-to-elsewhere/secret.txt`.
    let mut walked = root.path.clone();
    let components: Vec<&std::ffi::OsStr> =
        relative.components().map(Component::as_os_str).collect();
    let last = components.len().saturating_sub(1);
    for (index, component) in components.iter().enumerate() {
        walked.push(component);
        let meta = std::fs::symlink_metadata(&walked).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SourceViewError::NotFound {
                    at: display_relative(&root.path, &walked),
                }
            } else {
                SourceViewError::Io {
                    message: error.to_string(),
                }
            }
        })?;
        if meta.file_type().is_symlink() {
            return Err(SourceViewError::SymlinkRejected {
                at: display_relative(&root.path, &walked),
            });
        }
        if index < last && !meta.is_dir() {
            return Err(SourceViewError::NotAFile {
                at: display_relative(&root.path, &walked),
            });
        }
        if index == last && !meta.is_file() {
            return Err(SourceViewError::NotAFile {
                at: display_relative(&root.path, &walked),
            });
        }
    }

    // Belt and braces: the resolved path must still canonicalise inside the
    // root. The symlink walk above already guarantees this on a quiet
    // filesystem; this catches a race where a component was swapped mid-walk.
    let canonical = dunce::canonicalize(&absolute).map_err(|error| SourceViewError::Io {
        message: error.to_string(),
    })?;
    if !canonical.starts_with(&root.path) {
        return Err(SourceViewError::OutsideRoot);
    }

    Ok(ResolvedSource {
        root: root.clone(),
        relative_path: to_display_path(&relative),
        absolute_path: canonical,
    })
}

/// Lexically normalise `requested` into a root-relative path.
///
/// Absolute inputs are accepted only when they already sit under `root`.
/// `..` is refused outright rather than collapsed: collapsing would let
/// `a/../../b` look contained while naming something outside.
pub fn normalize_relative(root: &Path, requested: &str) -> Result<PathBuf, SourceViewError> {
    if requested.contains('\0') {
        return Err(SourceViewError::NulByte);
    }
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err(SourceViewError::EmptyPath);
    }

    let candidate = Path::new(trimmed);
    let relative_text: String = if candidate.is_absolute() {
        strip_root_prefix(root, candidate).ok_or(SourceViewError::AbsolutePathOutsideRoot)?
    } else {
        trimmed.to_string()
    };

    let mut normalized = PathBuf::new();
    for segment in split_segments(&relative_text) {
        match segment {
            "" | "." => continue,
            ".." => return Err(SourceViewError::ParentEscape),
            other => {
                // A segment must be a plain name. Anything std reads back as
                // a prefix, root, or parent is structurally unsafe here.
                let parsed = Path::new(other);
                let mut parts = parsed.components();
                match (parts.next(), parts.next()) {
                    (Some(Component::Normal(name)), None) => normalized.push(name),
                    _ => {
                        return Err(SourceViewError::InvalidComponent {
                            component: other.to_string(),
                        });
                    }
                }
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(SourceViewError::EmptyPath);
    }
    Ok(normalized)
}

#[cfg(windows)]
fn split_segments(input: &str) -> impl Iterator<Item = &str> {
    input.split(['/', '\\'])
}

#[cfg(not(windows))]
fn split_segments(input: &str) -> impl Iterator<Item = &str> {
    // A backslash is a legal filename byte on Unix, so it is never a separator.
    input.split('/')
}

/// Strip an approved root prefix from an absolute request, case-insensitively
/// on Windows where the same directory has several valid spellings.
fn strip_root_prefix(root: &Path, candidate: &Path) -> Option<String> {
    if let Ok(rest) = candidate.strip_prefix(root) {
        return Some(rest.to_string_lossy().into_owned());
    }
    if cfg!(windows) {
        let root_text = root.to_string_lossy().to_lowercase();
        let candidate_text = candidate.to_string_lossy().to_lowercase();
        let rest = candidate_text.strip_prefix(&root_text)?;
        let rest = rest.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return None;
        }
        // Re-slice the original so the returned segment keeps its real case.
        let start = candidate.to_string_lossy().len() - rest.len();
        return Some(candidate.to_string_lossy()[start..].to_string());
    }
    None
}

fn to_display_path(relative: &Path) -> String {
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn display_relative(root: &Path, absolute: &Path) -> String {
    absolute
        .strip_prefix(root)
        .map(to_display_path)
        .unwrap_or_else(|_| absolute.display().to_string())
}

/// FNV-1a (64-bit). Used only to give a root a stable, non-secret identity.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
