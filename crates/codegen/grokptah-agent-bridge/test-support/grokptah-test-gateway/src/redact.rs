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

/// Whether a header value should be hidden from default diagnostics.
///
/// Matching is intentionally conservative. Test logs do not need the value
/// of any header whose name suggests credentials or session authority.
pub fn is_sensitive_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "x-goog-api-key"
            | "x-auth-token"
    ) || name.contains("token")
        || name.contains("secret")
        || name.contains("credential")
        || name.ends_with("-key")
}

/// Copy headers while replacing sensitive values with `<redacted>`.
pub fn redacted_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_sensitive_header(name) {
                "<redacted>".to_owned()
            } else {
                value.clone()
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
    fn sensitive_header_matching_is_conservative() {
        for name in [
            "Authorization",
            "x-api-key",
            "Cookie",
            "x-session-token",
            "provider-secret",
        ] {
            assert!(is_sensitive_header(name), "expected {name} to be sensitive");
        }
        assert!(!is_sensitive_header("content-type"));
        assert!(!is_sensitive_header("x-request-id"));
    }
}
