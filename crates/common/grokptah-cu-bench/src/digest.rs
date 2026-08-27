//! Canonical serialization and bounded digests.
//!
//! Every artifact this crate emits is digested through exactly one path so
//! that a report produced on one machine is byte-identical to a report
//! produced on another. Two rules make that hold:
//!
//! 1. Canonical JSON only. `serde_json`'s default object representation is a
//!    `BTreeMap`, so key order is lexicographic, not insertion order.
//! 2. No floating point in anything digested. The benchmark models geometry
//!    in integer logical units, time in integer milliseconds, and confidence
//!    in coarse buckets. That is a deliberate restriction of the production
//!    `ObservationGeometry`, which uses `f64`: a benchmark that digests `f64`
//!    is not reproducible across targets, so the fixture layer trades a
//!    little fidelity for exact replay.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Length of every digest string this crate emits.
pub const DIGEST_HEX_LEN: usize = 64;

/// Hex-encoded SHA-256 of raw bytes.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut hex = String::with_capacity(DIGEST_HEX_LEN);
    for byte in out {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Canonical JSON text for a serializable value.
///
/// # Panics
/// Never for the types in this crate; every one of them is a plain data
/// struct with no map keys that can fail to serialize. The `expect` documents
/// that invariant rather than propagating an error nobody can act on.
#[must_use]
pub fn canonical_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("benchmark artifacts are always serializable")
}

/// Pretty canonical JSON, for artifacts that are read by humans and diffed in
/// review. Key order is still lexicographic, so the digest of the pretty form
/// is as stable as the compact form.
#[must_use]
pub fn canonical_json_pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("benchmark artifacts are always serializable")
}

/// Digest of the canonical JSON encoding of a value.
#[must_use]
pub fn digest_of<T: Serialize>(value: &T) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

/// Fold an ordered list of digests into one digest.
///
/// Domain-separated with a length prefix so that `["ab","cd"]` and `["abcd"]`
/// cannot collide.
#[must_use]
pub fn fold_digests(domain: &str, parts: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\x00");
    hasher.update((parts.len() as u64).to_be_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let out = hasher.finalize();
    let mut hex = String::with_capacity(DIGEST_HEX_LEN);
    for byte in out {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// True when `value` is a well-formed digest emitted by this crate.
#[must_use]
pub fn is_digest(value: &str) -> bool {
    value.len() == DIGEST_HEX_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_lowercase_hex_of_fixed_width() {
        let digest = sha256_hex(b"grokptah");
        assert_eq!(digest.len(), DIGEST_HEX_LEN);
        assert!(is_digest(&digest));
    }

    #[test]
    fn canonical_json_sorts_object_keys() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"zeta":1,"alpha":2,"mid":3}"#).unwrap();
        assert_eq!(canonical_json(&value), r#"{"alpha":2,"mid":3,"zeta":1}"#);
    }

    #[test]
    fn fold_is_length_prefixed_so_concatenation_cannot_collide() {
        let split = fold_digests("d", &["ab".into(), "cd".into()]);
        let joined = fold_digests("d", &["abcd".into()]);
        assert_ne!(split, joined);
    }

    #[test]
    fn fold_is_domain_separated() {
        let parts = vec![sha256_hex(b"x")];
        assert_ne!(fold_digests("a", &parts), fold_digests("b", &parts));
    }
}
