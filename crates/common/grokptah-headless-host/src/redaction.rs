//! Write-boundary redaction.
//!
//! This module *transforms* data on the way out of the host: it rewrites host
//! paths to stable labels, masks credential-shaped values, strips control
//! characters, and bounds size and depth. It deliberately runs before anything
//! is journaled, so an unredacted value is never durable, and again before any
//! projection is returned.
//!
//! It is a scrubber, not a leak detector: it answers "what is safe to publish",
//! not "did something leak". Detection lives with the contract owner.

use serde_json::{Map, Value};

/// Replacement emitted in place of a credential-shaped value.
pub const REDACTED: &str = "<redacted>";
/// Label substituted for the host home prefix.
pub const HOME_LABEL: &str = "<home>";
/// Label substituted for the approved workspace prefix.
pub const WORKSPACE_LABEL: &str = "<workspace>";

/// Maximum nesting depth retained in a redacted structured value.
pub const MAX_VALUE_DEPTH: usize = 8;
/// Maximum array entries retained in a redacted structured value.
pub const MAX_ARRAY_ENTRIES: usize = 256;
/// Maximum object entries retained in a redacted structured value.
pub const MAX_OBJECT_ENTRIES: usize = 256;
/// Maximum UTF-8 bytes retained in one redacted string.
pub const MAX_STRING_BYTES: usize = 8 * 1024;

/// Issuer prefixes whose bounded suffix is treated as a live credential.
const ISSUER_PREFIXES: [&str; 9] = [
    "sk-",
    "xai-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "AKIA",
    "xoxb-",
    "xoxp-",
];

/// Minimum run length before an issuer-prefixed token is treated as a secret.
const MIN_ISSUER_TOKEN_BYTES: usize = 20;

/// Host path labels and credential rules applied at every write boundary.
#[derive(Debug, Clone)]
pub struct RedactionPolicy {
    prefixes: Vec<(String, &'static str)>,
}

impl RedactionPolicy {
    /// Build a policy that hides the host home and approved workspace roots.
    ///
    /// The longest prefix wins, so a nested root cannot leak through the
    /// shorter label.
    pub fn new(home: &str, workspace: &str) -> Self {
        let mut prefixes = Vec::new();
        if !home.is_empty() {
            prefixes.push((home.to_owned(), HOME_LABEL));
        }
        if !workspace.is_empty() {
            prefixes.push((workspace.to_owned(), WORKSPACE_LABEL));
        }
        prefixes.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
        Self { prefixes }
    }

    /// A policy with no host roots, for callers that only need credential and
    /// control-character scrubbing.
    pub fn bare() -> Self {
        Self {
            prefixes: Vec::new(),
        }
    }

    /// Scrub one string and report whether it was shortened.
    pub fn scrub_bounded(&self, input: &str, max_bytes: usize) -> (String, bool) {
        let labelled = self.apply_labels(input);
        let masked = mask_credentials(&labelled);
        let cleaned = strip_control(&masked);
        truncate_utf8(cleaned, max_bytes)
    }

    /// Scrub one string using the default string bound.
    pub fn scrub(&self, input: &str) -> String {
        self.scrub_bounded(input, MAX_STRING_BYTES).0
    }

    /// Scrub a structured value, bounding depth, width, and string size.
    pub fn scrub_value(&self, value: &Value) -> Value {
        self.scrub_value_at(value, 0)
    }

    fn scrub_value_at(&self, value: &Value, depth: usize) -> Value {
        if depth >= MAX_VALUE_DEPTH {
            return Value::String("<omitted:depth>".to_owned());
        }
        match value {
            Value::String(text) => Value::String(self.scrub(text)),
            Value::Array(entries) => {
                let mut out: Vec<Value> = entries
                    .iter()
                    .take(MAX_ARRAY_ENTRIES)
                    .map(|entry| self.scrub_value_at(entry, depth + 1))
                    .collect();
                if entries.len() > MAX_ARRAY_ENTRIES {
                    out.push(Value::String(format!(
                        "<omitted:{} more>",
                        entries.len() - MAX_ARRAY_ENTRIES
                    )));
                }
                Value::Array(out)
            }
            Value::Object(entries) => {
                let mut out = Map::new();
                for (key, entry) in entries.iter().take(MAX_OBJECT_ENTRIES) {
                    let key = self.scrub(key);
                    if is_secret_key(&key) {
                        out.insert(key, Value::String(REDACTED.to_owned()));
                    } else {
                        out.insert(key, self.scrub_value_at(entry, depth + 1));
                    }
                }
                if entries.len() > MAX_OBJECT_ENTRIES {
                    out.insert(
                        "<omitted>".to_owned(),
                        Value::from(entries.len() - MAX_OBJECT_ENTRIES),
                    );
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }

    fn apply_labels(&self, input: &str) -> String {
        let mut out = input.to_owned();
        for (prefix, label) in &self.prefixes {
            if out.contains(prefix.as_str()) {
                out = out.replace(prefix.as_str(), label);
            }
        }
        out
    }
}

/// Validate a bounded, repository-relative path for a review projection.
///
/// Absolute paths, traversal, Windows separators, and control characters are
/// rejected rather than rewritten, so a caller cannot smuggle a host path in.
pub fn relative_path(path: &str, max_bytes: usize) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.len() > max_bytes {
        return None;
    }
    if trimmed.starts_with('/') || trimmed.contains('\\') || trimmed.contains(':') {
        return None;
    }
    if trimmed.split('/').any(|segment| segment == "..") {
        return None;
    }
    if trimmed.chars().any(|character| character.is_control()) {
        return None;
    }
    Some(trimmed.to_owned())
}

fn is_secret_key(key: &str) -> bool {
    const EXACT: [&str; 12] = [
        "authorization",
        "apikey",
        "api_key",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "credentials",
        "cookie",
        "private_key",
        "session_key",
    ];
    let lowered = key.to_ascii_lowercase();
    if EXACT.contains(&lowered.as_str()) {
        return true;
    }
    lowered.ends_with("_token")
        || lowered.ends_with("_key")
        || lowered.ends_with("_secret")
        || lowered.ends_with("_password")
        || lowered.ends_with("apikey")
}

/// `=` and `:` are deliberately excluded so `NAME=value` and `Header: value`
/// split into a name and a value rather than fusing into one opaque run.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'/' | b'.')
}

/// Whether `lowered` ends with `word` as its own trailing component, so
/// `XAI_API_KEY` matches `key` but `monkey` does not.
fn ends_with_secret_word(lowered: &str, word: &str) -> bool {
    lowered == word
        || lowered
            .strip_suffix(word)
            .is_some_and(|head| head.ends_with(['_', '-', '.']))
}

fn is_secret_assignment_name(run: &str) -> bool {
    let lowered = run.to_ascii_lowercase();
    if lowered == "bearer" || is_secret_key(&lowered) {
        return true;
    }
    ["key", "token", "secret", "password", "credential"]
        .iter()
        .any(|word| ends_with_secret_word(&lowered, word))
}

/// Mask credential-shaped runs: issuer-prefixed tokens, `NAME=value` secret
/// assignments, and `Authorization`/`Bearer` values.
fn mask_credentials(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut index = 0usize;
    // Set when the previous run named a secret and the separator was seen, so
    // the next run is the value that must be masked.
    let mut mask_next_run = false;

    while index < bytes.len() {
        if !is_token_byte(bytes[index]) {
            let separator = bytes[index];
            out.push(separator as char);
            if mask_next_run && !matches!(separator, b' ' | b'\t' | b'"' | b'\'' | b'=' | b':') {
                mask_next_run = false;
            }
            index += 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && is_token_byte(bytes[index]) {
            index += 1;
        }
        let run = &input[start..index];

        if mask_next_run {
            if is_secret_assignment_name(run) {
                // `Authorization: Bearer <token>` — `Bearer` is the scheme, so
                // the run after it is still the secret.
                out.push_str(run);
                mask_next_run = bytes
                    .get(index)
                    .is_some_and(|byte| matches!(byte, b'=' | b':' | b' ' | b'\t'));
                continue;
            }
            out.push_str(REDACTED);
            mask_next_run = false;
            continue;
        }

        let issuer_secret = ISSUER_PREFIXES
            .iter()
            .any(|prefix| run.starts_with(prefix) && run.len() >= MIN_ISSUER_TOKEN_BYTES);
        if issuer_secret {
            out.push_str(REDACTED);
            continue;
        }

        out.push_str(run);
        if is_secret_assignment_name(run) {
            let followed_by_separator = bytes
                .get(index)
                .is_some_and(|byte| matches!(byte, b'=' | b':' | b' ' | b'\t'));
            mask_next_run = followed_by_separator;
        }
    }

    out
}

fn strip_control(input: &str) -> String {
    input
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

fn truncate_utf8(input: String, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input, false);
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    (input[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> RedactionPolicy {
        RedactionPolicy::new("/private/hosts/headless", "/private/hosts/project")
    }

    #[test]
    fn host_roots_become_stable_labels() {
        let scrubbed = policy()
            .scrub("wrote /private/hosts/project/src/main.rs from /private/hosts/headless/runs");
        assert_eq!(scrubbed, "wrote <workspace>/src/main.rs from <home>/runs");
        assert!(!scrubbed.contains("/private/hosts"));
    }

    #[test]
    fn issuer_tokens_and_secret_assignments_are_masked() {
        let scrubbed =
            policy().scrub("export XAI_API_KEY=xai-abcdefghijklmnopqrstuvwxyz012345 done");
        assert!(!scrubbed.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(scrubbed.contains(REDACTED));
        assert!(scrubbed.ends_with(" done"));

        let header = policy().scrub("Authorization: Bearer abcdefghijklmnopqrstuvwx");
        assert!(!header.contains("abcdefghijklmnopqrstuvwx"));
    }

    #[test]
    fn ordinary_text_and_token_counts_survive() {
        let scrubbed = policy().scrub("used 1200 tokens over 3 rounds; fingerprint sha256:abc123");
        assert_eq!(
            scrubbed,
            "used 1200 tokens over 3 rounds; fingerprint sha256:abc123"
        );
        // A word that merely ends in a secret word is not a secret name.
        assert_eq!(policy().scrub("monkey business"), "monkey business");
        assert_eq!(policy().scrub("keystone habits"), "keystone habits");
    }

    #[test]
    fn secret_object_keys_are_replaced_whole() {
        let value = policy().scrub_value(&json!({
            "access_token": "not-issuer-prefixed-but-secret",
            "token_count": 42,
            "note": "at /private/hosts/project/a.rs"
        }));
        assert_eq!(value["access_token"], REDACTED);
        assert_eq!(value["token_count"], 42);
        assert_eq!(value["note"], "at <workspace>/a.rs");
    }

    #[test]
    fn structure_is_bounded_and_says_what_it_dropped() {
        let deep = (0..MAX_VALUE_DEPTH + 3).fold(json!("leaf"), |acc, _| json!({ "next": acc }));
        let scrubbed = policy().scrub_value(&deep);
        assert!(
            serde_json::to_string(&scrubbed)
                .expect("serializes")
                .contains("<omitted:depth>")
        );

        let wide = Value::Array(vec![json!("x"); MAX_ARRAY_ENTRIES + 5]);
        let scrubbed = policy().scrub_value(&wide);
        let entries = scrubbed.as_array().expect("array");
        assert_eq!(entries.len(), MAX_ARRAY_ENTRIES + 1);
        assert_eq!(entries[MAX_ARRAY_ENTRIES], "<omitted:5 more>");
    }

    #[test]
    fn control_characters_are_stripped_and_bounds_are_reported() {
        let scrubbed = policy().scrub("line\u{0007}one\nline two");
        assert_eq!(scrubbed, "lineone\nline two");
        let (short, truncated) = policy().scrub_bounded("abcdefgh", 4);
        assert_eq!(short, "abcd");
        assert!(truncated);
    }

    #[test]
    fn relative_paths_reject_host_and_traversal_shapes() {
        assert_eq!(
            relative_path("src/main.rs", 256).as_deref(),
            Some("src/main.rs")
        );
        assert!(relative_path("/etc/passwd", 256).is_none());
        assert!(relative_path("../secret", 256).is_none());
        assert!(relative_path("C:\\win", 256).is_none());
        assert!(relative_path("a\nb", 256).is_none());
    }
}
