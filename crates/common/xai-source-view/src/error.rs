//! Closed, structured refusal contract for read-only source inspection.
//!
//! Two invariants hold for every variant here:
//!
//! * **The set is closed.** [`SourceViewError::CODES`] lists every code, and a
//!   round-trip test asserts it matches the enum. The TypeScript parser and
//!   the JSON Schema are checked against the same list, so a code cannot be
//!   added on one side only.
//! * **No variant leaks an absolute path or file content.** Refusals carry a
//!   root-relative segment, a bounded digest, or nothing at all. An error
//!   message must never become a way to probe the host filesystem or read
//!   bytes the caller was refused.

use std::fmt;

/// Why a source-inspection request was refused.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(
    tag = "code",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum SourceViewError {
    // -- authorization ------------------------------------------------------
    /// The principal has no inspectable boundary at all.
    NoApprovedRoot,
    /// The snapshot named by a token is unknown: swept, revoked, or forged.
    SnapshotUnknown,
    /// The token is not a well-formed source-view token.
    TokenMalformed,
    /// The token's authentication tag does not verify under the store key.
    TokenSignatureInvalid,
    /// The token is past its absolute deadline.
    TokenExpired,
    /// The token's snapshot was explicitly revoked.
    TokenRevoked,
    /// The acting principal is not the principal the snapshot was issued to.
    PrincipalMismatch,
    /// Authorization inputs changed after the snapshot was issued.
    PolicyDrift,
    /// The token names a root that is not in its own snapshot.
    UnknownRoot,

    // -- containment --------------------------------------------------------
    EmptyPath,
    NulByte,
    /// An absolute request that does not lie under the named root.
    AbsolutePathOutsideRoot,
    /// The request walks above its root with `..`.
    ParentEscape,
    /// A component is structurally unusable for a contained read.
    InvalidComponent {
        segment: String,
    },
    /// A Windows reserved device name (`CON`, `NUL`, `COM1`, …).
    ReservedDeviceName {
        segment: String,
    },
    /// A Windows alternate data stream (`file.txt:stream`).
    AlternateDataStream {
        segment: String,
    },
    /// A path form this boundary refuses outright: UNC shares, the device
    /// namespaces (`\\?\`, `\\.\`), and drive-relative paths.
    UnsupportedPathForm,
    /// A component on the resolved path is a symbolic link. Links are never
    /// followed: a link inside a root can still point outside it.
    SymlinkRejected {
        segment: String,
    },
    /// A Windows reparse point (junction, mount point, symlink) was opened
    /// with no-follow semantics and refused.
    ReparsePointRejected {
        segment: String,
    },
    NotFound {
        segment: String,
    },
    NotAFile {
        segment: String,
    },
    /// The opened handle did not resolve inside the root after opening.
    OutsideRoot,
    /// The root's on-disk identity changed between snapshot and action.
    RootIdentityChanged,
    /// The file's identity changed between opening and finishing the read.
    DocumentChanged,

    // -- bounds -------------------------------------------------------------
    TooLarge {
        byte_len: u64,
        max_bytes: u64,
    },
    /// A byte range that is empty, inverted, or past the supported ceiling.
    RangeInvalid,
    /// A continuation cursor that does not belong to this document.
    CursorInvalid,

    // -- environment --------------------------------------------------------
    RootUnavailable,
    Io {
        detail: String,
    },
}

impl SourceViewError {
    /// Every code this contract can emit, in declaration order.
    ///
    /// The TypeScript parser, the JSON Schema, and the golden fixtures are all
    /// checked against this list.
    pub const CODES: &'static [&'static str] = &[
        "no_approved_root",
        "snapshot_unknown",
        "token_malformed",
        "token_signature_invalid",
        "token_expired",
        "token_revoked",
        "principal_mismatch",
        "policy_drift",
        "unknown_root",
        "empty_path",
        "nul_byte",
        "absolute_path_outside_root",
        "parent_escape",
        "invalid_component",
        "reserved_device_name",
        "alternate_data_stream",
        "unsupported_path_form",
        "symlink_rejected",
        "reparse_point_rejected",
        "not_found",
        "not_a_file",
        "outside_root",
        "root_identity_changed",
        "document_changed",
        "too_large",
        "range_invalid",
        "cursor_invalid",
        "root_unavailable",
        "io",
    ];

    /// Stable machine code, mirrored by the TypeScript parser and the schema.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoApprovedRoot => "no_approved_root",
            Self::SnapshotUnknown => "snapshot_unknown",
            Self::TokenMalformed => "token_malformed",
            Self::TokenSignatureInvalid => "token_signature_invalid",
            Self::TokenExpired => "token_expired",
            Self::TokenRevoked => "token_revoked",
            Self::PrincipalMismatch => "principal_mismatch",
            Self::PolicyDrift => "policy_drift",
            Self::UnknownRoot => "unknown_root",
            Self::EmptyPath => "empty_path",
            Self::NulByte => "nul_byte",
            Self::AbsolutePathOutsideRoot => "absolute_path_outside_root",
            Self::ParentEscape => "parent_escape",
            Self::InvalidComponent { .. } => "invalid_component",
            Self::ReservedDeviceName { .. } => "reserved_device_name",
            Self::AlternateDataStream { .. } => "alternate_data_stream",
            Self::UnsupportedPathForm => "unsupported_path_form",
            Self::SymlinkRejected { .. } => "symlink_rejected",
            Self::ReparsePointRejected { .. } => "reparse_point_rejected",
            Self::NotFound { .. } => "not_found",
            Self::NotAFile { .. } => "not_a_file",
            Self::OutsideRoot => "outside_root",
            Self::RootIdentityChanged => "root_identity_changed",
            Self::DocumentChanged => "document_changed",
            Self::TooLarge { .. } => "too_large",
            Self::RangeInvalid => "range_invalid",
            Self::CursorInvalid => "cursor_invalid",
            Self::RootUnavailable => "root_unavailable",
            Self::Io { .. } => "io",
        }
    }

    /// True when the refusal is about *who is asking*, not *what was asked*.
    ///
    /// The desktop uses this to decide whether to re-request a snapshot or to
    /// surface a hard stop: an authorization refusal is never retried with the
    /// same token.
    pub fn is_authorization(&self) -> bool {
        matches!(
            self,
            Self::NoApprovedRoot
                | Self::SnapshotUnknown
                | Self::TokenMalformed
                | Self::TokenSignatureInvalid
                | Self::TokenExpired
                | Self::TokenRevoked
                | Self::PrincipalMismatch
                | Self::PolicyDrift
                | Self::UnknownRoot
        )
    }

    /// Build an IO refusal whose detail is the error *kind*, never the
    /// operating system's message: OS strings routinely embed the full path.
    pub fn io(error: &std::io::Error) -> Self {
        Self::Io {
            detail: io_kind_label(error).to_string(),
        }
    }
}

/// A bounded, path-free label for an IO failure.
fn io_kind_label(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::InvalidInput => "invalid_input",
        ErrorKind::InvalidData => "invalid_data",
        ErrorKind::TimedOut => "timed_out",
        ErrorKind::Interrupted => "interrupted",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        _ => "unavailable",
    }
}

impl fmt::Display for SourceViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoApprovedRoot => f.write_str("no workspace is approved for source inspection"),
            Self::SnapshotUnknown => {
                f.write_str("the authorization snapshot is no longer available")
            }
            Self::TokenMalformed => f.write_str("the source token is not well formed"),
            Self::TokenSignatureInvalid => f.write_str("the source token failed verification"),
            Self::TokenExpired => f.write_str("the source token has expired"),
            Self::TokenRevoked => f.write_str("the source token was revoked"),
            Self::PrincipalMismatch => {
                f.write_str("the source token belongs to a different principal")
            }
            Self::PolicyDrift => f.write_str("authorization changed after the snapshot was issued"),
            Self::UnknownRoot => f.write_str("the source token names an unapproved root"),
            Self::EmptyPath => f.write_str("the requested path is empty"),
            Self::NulByte => f.write_str("the requested path contains a NUL byte"),
            Self::AbsolutePathOutsideRoot => {
                f.write_str("the requested path is outside the approved root")
            }
            Self::ParentEscape => f.write_str("the requested path walks above the approved root"),
            Self::InvalidComponent { segment } => {
                write!(f, "path segment `{segment}` is not readable in place")
            }
            Self::ReservedDeviceName { segment } => {
                write!(f, "path segment `{segment}` is a reserved device name")
            }
            Self::AlternateDataStream { segment } => {
                write!(f, "path segment `{segment}` names an alternate data stream")
            }
            Self::UnsupportedPathForm => {
                f.write_str("that path form is not readable through this boundary")
            }
            Self::SymlinkRejected { segment } => {
                write!(
                    f,
                    "`{segment}` is a symbolic link; links are never followed"
                )
            }
            Self::ReparsePointRejected { segment } => {
                write!(
                    f,
                    "`{segment}` is a reparse point; links are never followed"
                )
            }
            Self::NotFound { segment } => write!(f, "`{segment}` is not in the approved root"),
            Self::NotAFile { segment } => write!(f, "`{segment}` is not a regular file"),
            Self::OutsideRoot => f.write_str("the opened file is outside the approved root"),
            Self::RootIdentityChanged => {
                f.write_str("the approved root changed identity since it was authorized")
            }
            Self::DocumentChanged => f.write_str("the file changed while it was being read"),
            Self::TooLarge {
                byte_len,
                max_bytes,
            } => write!(f, "file is {byte_len} bytes; the ceiling is {max_bytes}"),
            Self::RangeInvalid => f.write_str("the requested byte range is not readable"),
            Self::CursorInvalid => f.write_str("the continuation cursor does not match this file"),
            Self::RootUnavailable => f.write_str("the approved root is not readable"),
            Self::Io { detail } => write!(f, "filesystem read failed: {detail}"),
        }
    }
}

impl std::error::Error for SourceViewError {}

/// Format a refusal for a transport boundary as `code: sentence`.
///
/// The code is stable and parsed by callers; the sentence is for people. Both
/// halves are free of absolute paths and of file content.
pub fn boundary_message(error: &SourceViewError) -> String {
    format!("{}: {error}", error.code())
}
