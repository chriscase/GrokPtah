//! Share-safe guards for public projections.
//!
//! Public projections cross a product boundary. The authority may hold
//! credentials, absolute host paths, and provider endpoints; none of them may
//! appear in what a broker, browser, or non-Rust consumer receives.
//!
//! Two strictness levels exist because the two kinds of field carry different
//! risk:
//!
//! * [`ensure_share_safe_metadata`] guards *authority-generated* metadata
//!   (identities, reasons, dispositions, event kinds, repository-relative
//!   paths). The authority controls every byte, so credentials, URLs, absolute
//!   paths, traversal, and control characters are all rejected.
//! * [`ensure_no_credential_material`] guards *content* the user or model
//!   produced (prompt previews, diffs, summaries, event payloads). A diff may
//!   legitimately contain a URL or an absolute path, so only credential-shaped
//!   material is rejected — that redaction invariant holds regardless of origin.
//!
//! Both are deterministic, allocation-light, and dependency-free.

use std::fmt;

/// Why a public projection was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeakKind {
    /// Credential-shaped material (bearer token, API key, private key block).
    Credential,
    /// An absolute host path or Windows drive/UNC path.
    AbsolutePath,
    /// A parent-directory traversal segment.
    ParentEscape,
    /// A provider or transport URL.
    Url,
    /// A disallowed C0 control character.
    ControlCharacter,
    /// The value exceeded its declared byte bound.
    Oversized,
    /// The value was empty where the contract requires content.
    Empty,
}

impl LeakKind {
    /// Stable, share-safe reason code for an [`crate::ErrorEnvelope`].
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Credential => "credential_material",
            Self::AbsolutePath => "absolute_path",
            Self::ParentEscape => "parent_escape",
            Self::Url => "provider_url",
            Self::ControlCharacter => "control_character",
            Self::Oversized => "oversized",
            Self::Empty => "empty",
        }
    }
}

/// A rejected public projection field.
///
/// The finding deliberately carries only the *field name* and the *kind*. It
/// never echoes the offending value, so the finding itself stays share-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeakFinding {
    /// Contract field that failed the guard.
    pub field: &'static str,
    /// Why it failed.
    pub kind: LeakKind,
}

impl LeakFinding {
    /// Build a finding for a field.
    pub const fn new(field: &'static str, kind: LeakKind) -> Self {
        Self { field, kind }
    }
}

impl fmt::Display for LeakFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} rejected: {}", self.field, self.kind.reason_code())
    }
}

/// Unambiguous credential markers. These name credential *material*, not topic
/// words, so ordinary prose about authentication is not rejected.
const CREDENTIAL_MARKERS: &[&str] = &[
    "-----begin",
    "bearer ",
    "authorization:",
    "api_key=",
    "apikey=",
    "api-key:",
    "access_token=",
    "refresh_token=",
    "client_secret=",
    "private_key=",
    "aws_secret_access_key",
];

/// Issuer prefixes for opaque keys.
///
/// A prefix alone is too weak to act on: `sk-` also starts `sk-module.rs` and
/// `akia` also starts an ordinary identifier. A real key of these families
/// always carries a long opaque suffix, so a match additionally requires
/// [`MIN_KEY_SUFFIX`] trailing token characters.
const CREDENTIAL_PREFIXES: &[&str] = &["sk-", "xai-", "ghp_", "gho_", "github_pat_", "akia"];

/// Minimum opaque characters after an issuer prefix before it counts as a key.
const MIN_KEY_SUFFIX: usize = 12;

/// Transport/provider URL schemes that must not appear in authority metadata.
const URL_NEEDLES: &[&str] = &["http://", "https://", "ws://", "wss://", "file://"];

/// True when `needle` occurs in `haystack` at a token boundary.
///
/// A boundary is required so that short prefixes such as `sk-` match
/// `sk-live-abc` but not `risk-register`. Needles that already start with a
/// non-alphanumeric byte are matched literally.
fn contains_token(haystack_lower: &str, needle: &str) -> bool {
    let bytes = haystack_lower.as_bytes();
    let needs_boundary = needle
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    let mut from = 0usize;
    while let Some(offset) = haystack_lower[from..].find(needle) {
        let start = from + offset;
        if !needs_boundary {
            return true;
        }
        let boundary = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if boundary {
            return true;
        }
        from = start + 1;
        if from >= haystack_lower.len() {
            break;
        }
    }
    false
}

fn has_credential_material(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    if CREDENTIAL_MARKERS
        .iter()
        .any(|marker| contains_token(&lowered, marker))
    {
        return true;
    }
    CREDENTIAL_PREFIXES
        .iter()
        .any(|prefix| has_issuer_key(&lowered, prefix))
}

/// True when `prefix` appears at a token boundary followed by a long enough
/// opaque suffix to be a real key rather than an ordinary identifier.
fn has_issuer_key(haystack_lower: &str, prefix: &str) -> bool {
    let bytes = haystack_lower.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = haystack_lower[from..].find(prefix) {
        let start = from + offset;
        let boundary = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if boundary {
            let suffix = &bytes[start + prefix.len()..];
            let opaque = suffix
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'-' || **byte == b'_')
                .count();
            if opaque >= MIN_KEY_SUFFIX {
                return true;
            }
        }
        from = start + 1;
        if from >= haystack_lower.len() {
            break;
        }
    }
    false
}

fn has_url(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    URL_NEEDLES.iter().any(|needle| lowered.contains(needle))
}

fn has_absolute_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with("~/") || value.starts_with("\\\\") {
        return true;
    }
    // Windows drive-qualified path such as `C:\Users` or `c:/Users`.
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn has_parent_escape(value: &str) -> bool {
    value.split(['/', '\\']).any(|segment| segment == "..")
}

fn has_metadata_control_char(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

fn has_content_control_char(value: &str) -> bool {
    value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
}

/// Guard an authority-generated metadata string.
///
/// Rejects credential material, provider URLs, absolute paths, parent escapes,
/// control characters, empty values, and values above `max_bytes`.
pub fn ensure_share_safe_metadata(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), LeakFinding> {
    if value.trim().is_empty() {
        return Err(LeakFinding::new(field, LeakKind::Empty));
    }
    if value.len() > max_bytes {
        return Err(LeakFinding::new(field, LeakKind::Oversized));
    }
    if has_metadata_control_char(value) {
        return Err(LeakFinding::new(field, LeakKind::ControlCharacter));
    }
    if has_credential_material(value) {
        return Err(LeakFinding::new(field, LeakKind::Credential));
    }
    if has_url(value) {
        return Err(LeakFinding::new(field, LeakKind::Url));
    }
    if has_absolute_path(value) {
        return Err(LeakFinding::new(field, LeakKind::AbsolutePath));
    }
    if has_parent_escape(value) {
        return Err(LeakFinding::new(field, LeakKind::ParentEscape));
    }
    Ok(())
}

/// Guard user- or model-derived content for credential material only.
///
/// URLs and absolute paths are legitimate inside a diff or a prompt, so they
/// are allowed here; credential material never is.
pub fn ensure_no_credential_material(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), LeakFinding> {
    if value.len() > max_bytes {
        return Err(LeakFinding::new(field, LeakKind::Oversized));
    }
    if has_content_control_char(value) {
        return Err(LeakFinding::new(field, LeakKind::ControlCharacter));
    }
    if has_credential_material(value) {
        return Err(LeakFinding::new(field, LeakKind::Credential));
    }
    Ok(())
}

/// Recursively guard a JSON projection payload for credential material.
///
/// Object keys and string values are both scanned; numbers, booleans, and null
/// carry no text. `max_bytes` bounds the serialized payload.
pub fn ensure_json_share_safe(
    field: &'static str,
    value: &serde_json::Value,
    max_bytes: usize,
) -> Result<(), LeakFinding> {
    let encoded =
        serde_json::to_vec(value).map_err(|_| LeakFinding::new(field, LeakKind::Oversized))?;
    if encoded.len() > max_bytes {
        return Err(LeakFinding::new(field, LeakKind::Oversized));
    }
    scan_json(field, value)
}

fn scan_json(field: &'static str, value: &serde_json::Value) -> Result<(), LeakFinding> {
    match value {
        serde_json::Value::String(text) => {
            if has_credential_material(text) {
                return Err(LeakFinding::new(field, LeakKind::Credential));
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scan_json(field, item)?;
            }
            Ok(())
        }
        serde_json::Value::Object(entries) => {
            for (key, item) in entries {
                if has_credential_material(key) {
                    return Err(LeakFinding::new(field, LeakKind::Credential));
                }
                scan_json(field, item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_boundary_avoids_false_positives_on_ordinary_prose() {
        // `risk-register` contains "sk-" but not at a token boundary.
        assert!(ensure_no_credential_material("prompt", "update the risk-register", 512).is_ok());
        assert!(ensure_no_credential_material("prompt", "whisk-y notes", 512).is_ok());
        // A short suffix is an ordinary identifier, not a key.
        assert!(ensure_share_safe_metadata("path", "src/sk-module.rs", 512).is_ok());
        assert!(ensure_share_safe_metadata("path", "src/akiacode.rs", 512).is_ok());

        // A real key carries a long opaque suffix and must be rejected.
        assert_eq!(
            ensure_no_credential_material("prompt", "key sk-live-abc123def456", 512).unwrap_err(),
            LeakFinding::new("prompt", LeakKind::Credential)
        );
        assert_eq!(
            ensure_no_credential_material("prompt", "AKIAIOSFODNN7EXAMPLE", 512)
                .unwrap_err()
                .kind,
            LeakKind::Credential
        );
    }

    #[test]
    fn credential_material_is_rejected_in_content_and_metadata() {
        for probe in [
            "Authorization: Bearer abc",
            "-----BEGIN RSA PRIVATE KEY-----",
            "xai-abcdef0123456789",
            "github_pat_11ABCDEFGHIJKLMNOP",
            "api_key=zzz",
        ] {
            assert_eq!(
                ensure_no_credential_material("diff", probe, 4096)
                    .unwrap_err()
                    .kind,
                LeakKind::Credential,
                "content guard must reject {probe}"
            );
            assert_eq!(
                ensure_share_safe_metadata("reason", probe, 4096)
                    .unwrap_err()
                    .kind,
                LeakKind::Credential,
                "metadata guard must reject {probe}"
            );
        }
    }

    #[test]
    fn content_allows_urls_and_paths_but_metadata_does_not() {
        let diff = "+ fetch(\"https://example.test/x\")\n+ open(\"/etc/hosts\")";
        assert!(ensure_no_credential_material("diff", diff, 4096).is_ok());

        assert_eq!(
            ensure_share_safe_metadata("reason", "https://provider.test/v1", 4096)
                .unwrap_err()
                .kind,
            LeakKind::Url
        );
        assert_eq!(
            ensure_share_safe_metadata("path", "/Users/dev/secret", 4096)
                .unwrap_err()
                .kind,
            LeakKind::AbsolutePath
        );
        assert_eq!(
            ensure_share_safe_metadata("path", "C:\\Users\\dev", 4096)
                .unwrap_err()
                .kind,
            LeakKind::AbsolutePath
        );
        assert_eq!(
            ensure_share_safe_metadata("path", "src/../../etc", 4096)
                .unwrap_err()
                .kind,
            LeakKind::ParentEscape
        );
    }

    #[test]
    fn bounds_and_control_characters_fail_closed() {
        assert_eq!(
            ensure_share_safe_metadata("kind", "", 128)
                .unwrap_err()
                .kind,
            LeakKind::Empty
        );
        assert_eq!(
            ensure_share_safe_metadata("kind", "abcdef", 3)
                .unwrap_err()
                .kind,
            LeakKind::Oversized
        );
        assert_eq!(
            ensure_share_safe_metadata("kind", "line\nbreak", 128)
                .unwrap_err()
                .kind,
            LeakKind::ControlCharacter
        );
        // Content tolerates newlines/tabs but not other C0 controls.
        assert!(ensure_no_credential_material("diff", "a\nb\tc\r\n", 128).is_ok());
        assert_eq!(
            ensure_no_credential_material("diff", "a\u{0007}b", 128)
                .unwrap_err()
                .kind,
            LeakKind::ControlCharacter
        );
    }

    #[test]
    fn json_scan_reaches_nested_values_and_keys() {
        let nested = serde_json::json!({
            "outer": [{"inner": {"note": "token sk-live-abc123def456"}}]
        });
        assert_eq!(
            ensure_json_share_safe("update", &nested, 4096)
                .unwrap_err()
                .kind,
            LeakKind::Credential
        );

        let keyed = serde_json::json!({ "api_key=primary": "redacted" });
        assert_eq!(
            ensure_json_share_safe("update", &keyed, 4096)
                .unwrap_err()
                .kind,
            LeakKind::Credential
        );

        let safe = serde_json::json!({"kind": "tool_call", "count": 3, "ok": true, "n": null});
        assert!(ensure_json_share_safe("update", &safe, 4096).is_ok());

        let oversized = serde_json::json!({"blob": "x".repeat(200)});
        assert_eq!(
            ensure_json_share_safe("update", &oversized, 64)
                .unwrap_err()
                .kind,
            LeakKind::Oversized
        );
    }

    #[test]
    fn findings_never_echo_the_offending_value() {
        let finding = ensure_no_credential_material("prompt", "sk-live-supersecret-9f2", 512)
            .expect_err("credential must be rejected");
        let rendered = finding.to_string();
        assert!(!rendered.contains("supersecret"), "{rendered}");
        assert_eq!(rendered, "prompt rejected: credential_material");
    }
}
