//! Canonical digests.
//!
//! Every identity in this crate that could otherwise carry content -- an
//! objective, an element label, a typed value, a captured region -- is
//! reduced to a digest before it is allowed into a plan, a verdict, or a
//! receipt. A digest is comparable and reproducible without being readable,
//! which is exactly what the evidence path needs and exactly what a leak
//! needs it not to be.
//!
//! Digests are taken over *canonical* JSON: object keys sorted, no
//! insignificant whitespace, no floats. `serde_json` with a `BTreeMap`-backed
//! map preserves key order, and this crate uses no floating point in any
//! serialized structure, so the same value always produces the same bytes on
//! every platform.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Length of a hex-encoded SHA-256 digest.
pub const DIGEST_HEX_LEN: usize = 64;

/// Domain separator prefixes. Two different kinds of thing must never collide
/// on the same digest, so every digest is taken over `domain || 0x00 || bytes`.
pub mod domain {
    pub const OBJECTIVE: &str = "grokptah.cu.adaptive.objective.v1";
    pub const TEXT_PAYLOAD: &str = "grokptah.cu.adaptive.text.v1";
    pub const ELEMENT_ROLE: &str = "grokptah.cu.adaptive.role.v1";
    pub const REGION: &str = "grokptah.cu.adaptive.region.v1";
    pub const FRAME: &str = "grokptah.cu.adaptive.frame.v1";
    pub const PLAN: &str = "grokptah.cu.adaptive.plan.v1";
    pub const VERDICT: &str = "grokptah.cu.adaptive.verdict.v1";
    pub const TRACE: &str = "grokptah.cu.adaptive.trace.v1";
    pub const SUITE: &str = "grokptah.cu.adaptive.suite.v1";
}

/// Hex SHA-256 of `domain || 0x00 || bytes`.
#[must_use]
pub fn digest_bytes(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// Hex SHA-256 of a string under a domain.
#[must_use]
pub fn digest_str(domain: &str, value: &str) -> String {
    digest_bytes(domain, value.as_bytes())
}

/// Hex SHA-256 of a serializable value's canonical JSON form.
///
/// Returns `None` only if the value cannot be serialized at all, which for the
/// types in this crate is unreachable; callers fail closed rather than
/// substituting a placeholder digest.
#[must_use]
pub fn digest_canonical<T: Serialize>(domain: &str, value: &T) -> Option<String> {
    let bytes = serde_json::to_vec(value).ok()?;
    Some(digest_bytes(domain, &bytes))
}

/// True when `value` is a well-formed lowercase hex SHA-256 digest.
#[must_use]
pub fn is_digest(value: &str) -> bool {
    value.len() == DIGEST_HEX_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_are_stable_and_well_formed() {
        let a = digest_str(domain::OBJECTIVE, "rename the selected row");
        let b = digest_str(domain::OBJECTIVE, "rename the selected row");
        assert_eq!(a, b);
        assert!(is_digest(&a));
    }

    #[test]
    fn domains_separate_identical_inputs() {
        let same_input = "value";
        assert_ne!(
            digest_str(domain::OBJECTIVE, same_input),
            digest_str(domain::TEXT_PAYLOAD, same_input)
        );
    }

    #[test]
    fn domain_separator_cannot_be_forged_by_concatenation() {
        // Without the 0x00 separator, "ab" + "c" and "a" + "bc" would collide.
        assert_ne!(digest_str("ab", "c"), digest_str("a", "bc"));
    }

    #[test]
    fn rejects_uppercase_and_short_digests() {
        assert!(!is_digest(&"A".repeat(64)));
        assert!(!is_digest(&"a".repeat(63)));
        assert!(!is_digest(""));
    }
}
