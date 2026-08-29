//! Opaque identity types.
//!
//! Every identifier crossing this boundary is a validated newtype, not a bare
//! `String`. Two rules make that worth the ceremony:
//!
//! 1. **No raw host paths.** [`WorkspaceRef`] is an adapter-issued handle. The
//!    runtime's authorization still uses a canonical absolute workspace path,
//!    but that path is the adapter's private business. A consumer receives a
//!    ref and a non-sensitive label, and can never learn, construct, or
//!    forge a filesystem location from either. This mirrors how the Computer
//!    Use projection issues opaque `window_id` handles instead of OS pointers.
//! 2. **No path traversal in project-relative data.** [`RelativePath`] is the
//!    only path-shaped type on the boundary, and it rejects absolute paths,
//!    drive letters, UNC/verbatim prefixes, and `..` segments at construction.

use serde::{Deserialize, Serialize};

use crate::error::{SdkError, SdkErrorCode, SdkResult};

/// Longest identifier the seam accepts.
pub const MAX_ID_BYTES: usize = 128;
/// Longest project-relative path the seam accepts.
pub const MAX_RELATIVE_PATH_BYTES: usize = 1024;
/// Longest human label the seam accepts.
pub const MAX_LABEL_BYTES: usize = 200;

fn validate_id(field: &str, raw: &str) -> SdkResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SdkError::new(
            SdkErrorCode::InvalidRequest,
            format!("{field} must not be empty"),
        ));
    }
    if trimmed.len() > MAX_ID_BYTES {
        return Err(SdkError::new(
            SdkErrorCode::InvalidRequest,
            format!("{field} exceeds {MAX_ID_BYTES} bytes"),
        ));
    }
    // Deliberately narrow: an identifier that can hold a path separator, a
    // quote, or whitespace is an identifier that can be smuggled into a
    // filename, a URL, or a log line downstream.
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        return Err(SdkError::new(
            SdkErrorCode::InvalidRequest,
            format!("{field} may contain only ASCII letters, digits, '-', '_', '.', or ':'"),
        ));
    }
    if trimmed.contains("..") {
        return Err(SdkError::new(
            SdkErrorCode::InvalidRequest,
            format!("{field} must not contain '..'"),
        ));
    }
    Ok(trimmed.to_string())
}

macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        // Validation runs on decode too. A newtype that only checks its
        // constructor is a newtype a hostile or buggy host can bypass with a
        // JSON string.
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }

        impl $name {
            /// Validate and wrap. This is the only way to build one.
            pub fn new(raw: impl AsRef<str>) -> SdkResult<Self> {
                validate_id($field, raw.as_ref()).map(Self)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_id!(
    /// Durable execution-context identity. The product's "Lane" is a
    /// presentation projection of exactly one session; the seam carries only
    /// the session identity, matching the runtime's `lane_id == session_id`.
    SessionId,
    "sessionId"
);
opaque_id!(
    /// One finite model/tool execution.
    RunId,
    "runId"
);
opaque_id!(
    /// Durable agent identity.
    AgentId,
    "agentId"
);
opaque_id!(
    /// Durable work item identity.
    WorkId,
    "workId"
);
opaque_id!(
    /// One bounded, attributable claim to execute a work item.
    AttemptId,
    "attemptId"
);
opaque_id!(
    /// Caller-chosen idempotency key for exactly one mutation.
    RequestId,
    "requestId"
);
opaque_id!(
    /// Bounded artifact identity within a run or attempt.
    ArtifactId,
    "artifactId"
);
opaque_id!(
    /// Adapter-issued workspace handle. Never a filesystem path.
    ///
    /// The adapter maps this to the canonical workspace identity the runtime
    /// authorizes against. A consumer that has one ref cannot derive another,
    /// cannot learn where the workspace lives, and cannot address a workspace
    /// the host did not first advertise.
    WorkspaceRef,
    "workspaceRef"
);

/// A validated project-relative path.
///
/// Rejects anything that could escape a workspace or name a host location:
/// absolute paths, `..` segments, Windows drive letters, and UNC/verbatim
/// prefixes. Separators are normalized to `/` so a consumer renders the same
/// string regardless of the host OS.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RelativePath(String);

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl RelativePath {
    pub fn new(raw: impl AsRef<str>) -> SdkResult<Self> {
        let raw = raw.as_ref().trim();
        let reject = |why: &str| {
            Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                format!("relativePath {why}"),
            ))
        };
        if raw.is_empty() {
            return reject("must not be empty");
        }
        if raw.len() > MAX_RELATIVE_PATH_BYTES {
            return reject(&format!("exceeds {MAX_RELATIVE_PATH_BYTES} bytes"));
        }
        if raw.contains('\0') {
            return reject("must not contain a NUL byte");
        }
        let normalized = raw.replace('\\', "/");
        if normalized.starts_with('/') {
            return reject("must be relative, not absolute");
        }
        if normalized.starts_with("//") || normalized.starts_with("?/") {
            return reject("must not use a UNC or verbatim prefix");
        }
        // `C:/...`, `C:` — a drive-qualified path is absolute on Windows even
        // without a leading separator.
        let bytes = normalized.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return reject("must not be drive-qualified");
        }
        if normalized
            .split('/')
            .any(|segment| segment == ".." || segment == "~")
        {
            return reject("must not contain '..' or '~' segments");
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RelativePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A short, non-sensitive human label (a workspace nickname, a session title).
///
/// Bounded and stripped of control characters so a host cannot use a label to
/// inject terminal escapes or newlines into a consumer's UI or logs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Label(String);

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl Label {
    pub fn new(raw: impl AsRef<str>) -> SdkResult<Self> {
        let cleaned: String = raw
            .as_ref()
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .trim()
            .to_string();
        if cleaned.is_empty() {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "label must not be empty after removing control characters",
            ));
        }
        Ok(Self(crate::error::truncate_on_char_boundary(
            &cleaned,
            MAX_LABEL_BYTES,
        )))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_separators_traversal_and_whitespace() {
        for bad in [
            "",
            "  ",
            "has space",
            "has/slash",
            "has\\slash",
            "..",
            "a..b",
            "quote\"",
            "new\nline",
            "tab\there",
        ] {
            assert!(RunId::new(bad).is_err(), "{bad:?} must be rejected");
        }
        // Surrounding whitespace is trimmed, matching the runtime's own
        // credential-id handling; interior whitespace is still a rejection.
        assert_eq!(RunId::new(" run-1 ").unwrap().as_str(), "run-1");
        assert_eq!(RunId::new("run-1\n").unwrap().as_str(), "run-1");
    }

    #[test]
    fn ids_are_length_bounded() {
        assert!(RunId::new("a".repeat(MAX_ID_BYTES)).is_ok());
        assert!(RunId::new("a".repeat(MAX_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn relative_path_rejects_every_escape_shape() {
        for bad in [
            "/etc/passwd",
            "//server/share/x",
            "C:/Users/me/secret",
            "c:secret",
            "../../etc/passwd",
            "src/../../../etc/passwd",
            "~/private",
            "src/~/private",
            "",
        ] {
            assert!(RelativePath::new(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn relative_path_normalizes_separators() {
        assert_eq!(
            RelativePath::new("src\\app\\main.rs").unwrap().as_str(),
            "src/app/main.rs"
        );
    }

    #[test]
    fn labels_strip_control_characters_and_bound_length() {
        let label = Label::new("Build \u{1b}[31mred\u{1b}[0m\nlane").unwrap();
        assert!(!label.as_str().contains('\u{1b}'));
        assert!(!label.as_str().contains('\n'));
        assert!(Label::new("x".repeat(4096)).unwrap().as_str().len() <= MAX_LABEL_BYTES);
        assert!(Label::new("\u{1b}\n\t").is_err());
    }

    #[test]
    fn workspace_ref_is_not_constructible_from_a_path() {
        assert!(WorkspaceRef::new("/home/user/project").is_err());
        assert!(WorkspaceRef::new("ws-01HQ").is_ok());
    }
}
