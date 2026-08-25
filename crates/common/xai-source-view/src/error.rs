//! Fail-closed error contract for read-only source inspection.
//!
//! Every rejection carries a machine-readable `code` so the desktop can
//! explain the refusal without re-deriving policy in the view layer. No
//! variant carries credential material; path detail is limited to the
//! offending component so an error message cannot become an exfiltration
//! channel for content the caller was never allowed to read.

use std::fmt;

/// Why a source-inspection request was refused.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SourceViewError {
    /// No workspace or isolated worktree has been approved for inspection.
    NoApprovedRoot,
    /// The caller named a root that is not in the approved set.
    UnknownRoot { root_id: String },
    /// The request carried an empty or whitespace-only path.
    EmptyPath,
    /// The request carried an interior NUL, which no real path can contain.
    NulByte,
    /// An absolute path was supplied that does not lie under the approved root.
    AbsolutePathOutsideRoot,
    /// The path tried to walk above its root with `..`.
    ParentEscape,
    /// A path component was structurally invalid for a contained read.
    InvalidComponent { component: String },
    /// A component on the resolved path is a symlink. Links are never
    /// followed: a link inside an approved root can still point outside it.
    SymlinkRejected { at: String },
    /// Nothing exists at the resolved path.
    NotFound { at: String },
    /// The resolved path exists but is not a regular file.
    NotAFile { at: String },
    /// The resolved path left the approved root after resolution.
    OutsideRoot,
    /// The file is larger than the inspection ceiling allows.
    TooLarge { byte_len: u64, max_bytes: u64 },
    /// The approved root is not a directory that can be inspected.
    RootUnavailable { at: String },
    /// An underlying filesystem call failed.
    Io { message: String },
}

impl SourceViewError {
    /// Stable machine code, mirrored by the TypeScript parser tests.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoApprovedRoot => "no_approved_root",
            Self::UnknownRoot { .. } => "unknown_root",
            Self::EmptyPath => "empty_path",
            Self::NulByte => "nul_byte",
            Self::AbsolutePathOutsideRoot => "absolute_path_outside_root",
            Self::ParentEscape => "parent_escape",
            Self::InvalidComponent { .. } => "invalid_component",
            Self::SymlinkRejected { .. } => "symlink_rejected",
            Self::NotFound { .. } => "not_found",
            Self::NotAFile { .. } => "not_a_file",
            Self::OutsideRoot => "outside_root",
            Self::TooLarge { .. } => "too_large",
            Self::RootUnavailable { .. } => "root_unavailable",
            Self::Io { .. } => "io",
        }
    }
}

impl fmt::Display for SourceViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoApprovedRoot => f.write_str("no workspace is approved for source inspection"),
            Self::UnknownRoot { root_id } => {
                write!(f, "source root {root_id} is not approved for inspection")
            }
            Self::EmptyPath => f.write_str("the requested path is empty"),
            Self::NulByte => f.write_str("the requested path contains a NUL byte"),
            Self::AbsolutePathOutsideRoot => {
                f.write_str("the requested absolute path is outside the approved root")
            }
            Self::ParentEscape => {
                f.write_str("the requested path walks above the approved root with `..`")
            }
            Self::InvalidComponent { component } => {
                write!(f, "path component `{component}` is not readable in place")
            }
            Self::SymlinkRejected { at } => {
                write!(f, "`{at}` is a symbolic link; links are never followed")
            }
            Self::NotFound { at } => write!(f, "`{at}` does not exist in the approved root"),
            Self::NotAFile { at } => write!(f, "`{at}` is not a regular file"),
            Self::OutsideRoot => f.write_str("the resolved path is outside the approved root"),
            Self::TooLarge {
                byte_len,
                max_bytes,
            } => write!(
                f,
                "file is {byte_len} bytes; the view ceiling is {max_bytes}"
            ),
            Self::RootUnavailable { at } => {
                write!(f, "approved root `{at}` is not an inspectable directory")
            }
            Self::Io { message } => write!(f, "filesystem read failed: {message}"),
        }
    }
}

impl std::error::Error for SourceViewError {}

impl From<std::io::Error> for SourceViewError {
    fn from(value: std::io::Error) -> Self {
        Self::Io {
            message: value.to_string(),
        }
    }
}
