//! Secret-safe rendering helpers for test assertions and diagnostics.

use sha2::{Digest, Sha256};

/// Replace each non-empty secret with a stable, non-reversible marker.
pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut output = text.to_owned();
    for secret in secrets.iter().copied().filter(|secret| !secret.is_empty()) {
        let marker = format!("<redacted:{}>", short_hash(secret));
        output = output.replace(secret, &marker);
    }
    output
}

/// Whether a header value is safe to print in default diagnostics.
///
/// This is deliberately a small positive allowlist. Provider-specific,
/// identity, routing, request-ID, credential, and otherwise unknown headers
/// are redacted even when their names do not look secret.
pub fn is_printable_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "accept"
            | "accept-encoding"
            | "connection"
            | "content-length"
            | "content-type"
            | "transfer-encoding"
    )
}

/// Whether a header value is hidden from default diagnostics.
///
/// Kept as the inverse of [`is_printable_header`] for callers that want to
/// assert the diagnostic policy directly.
pub fn is_sensitive_header(name: &str) -> bool {
    !is_printable_header(name)
}

/// Copy headers while revealing values only from the positive allowlist.
pub fn redacted_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_printable_header(name) {
                value.clone()
            } else {
                "<redacted>".to_owned()
            };
            (name.clone(), value)
        })
        .collect()
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_redaction_is_stable_and_complete() {
        let rendered = redact("Bearer fixture-secret; fixture-secret", &["fixture-secret"]);
        assert!(!rendered.contains("fixture-secret"));
        assert_eq!(rendered.matches("<redacted:").count(), 2);
        assert_eq!(
            rendered,
            redact("Bearer fixture-secret; fixture-secret", &["fixture-secret"])
        );
    }

    #[test]
    fn printable_header_matching_is_a_small_positive_allowlist() {
        for name in [
            "Accept",
            "accept-encoding",
            "Connection",
            "content-length",
            "CONTENT-TYPE",
            "transfer-encoding",
        ] {
            assert!(is_printable_header(name), "expected {name} to be printable");
            assert!(!is_sensitive_header(name));
        }

        for name in [
            "Authorization",
            "x-api-key",
            "Cookie",
            "x-session-token",
            "provider-secret",
            "x-userid",
            "x-teamid",
            "x-request-id",
            "request-id",
            "host",
            "x-forwarded-host",
            "x-client-version",
            "user-agent",
        ] {
            assert!(is_sensitive_header(name), "expected {name} to be sensitive");
            assert!(!is_printable_header(name));
        }
    }
}
