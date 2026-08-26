//! Domain-separated, injective digests over exact bytes.
//!
//! Every digest in Semantic Help commits to the *bytes* it describes, not to a
//! name that currently resolves to them. Two properties make that true:
//!
//! * **Length prefixing.** Fields are encoded as `<utf8_len>:<field>`. Joining
//!   with a separator instead — `id|path#heading` — is not injective: a
//!   separator occurring inside a field makes the parse ambiguous, so two
//!   distinct field lists can hash identically. An attacker who controls one
//!   field could then forge another record's digest.
//! * **Domain separation.** The record kind is hashed first, itself length
//!   prefixed. A chunk id and a source id that happen to be the same string
//!   still land in different digest spaces, so a digest minted for one kind of
//!   record can never be replayed as another.
//!
//! This module is the single definition of those rules. The TypeScript side
//! re-implements `sha256` only because a browser bundle cannot call this code;
//! `parity` fixtures pin the two implementations to identical output.

use sha2::{Digest, Sha256};

/// Digest domains. Each record kind gets its own separated space.
pub mod domain {
    pub const SOURCE: &str = "grokptah.help.source.v1";
    pub const ARTICLE: &str = "grokptah.help.article.v1";
    pub const CHUNK: &str = "grokptah.help.chunk.v1";
    pub const SOURCE_SET: &str = "grokptah.help.source-set.v1";
    pub const CORPUS: &str = "grokptah.help.corpus.v1";
    pub const MANIFEST: &str = "grokptah.help.manifest.v1";
    pub const GRANT: &str = "grokptah.help.grant.v1";
    pub const ADMISSION: &str = "grokptah.help.admission.v1";
    pub const REQUEST: &str = "grokptah.help.request.v1";
    pub const RECEIPT: &str = "grokptah.help.receipt.v1";
    pub const CLAIM: &str = "grokptah.help.claim.v1";
}

/// Lowercase hex SHA-256 of the UTF-8 bytes of `value`.
#[must_use]
pub fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Lowercase hex SHA-256 of exact bytes.
#[must_use]
pub fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("{:x}", hasher.finalize())
}

/// Injective, length-prefixed encoding of a field list.
///
/// Each field becomes `<utf8ByteLength>:<field>`. Because the reader learns
/// each field's length before its bytes, no field content can be mistaken for
/// a delimiter and no two distinct lists share an encoding.
#[must_use]
pub fn length_prefixed(fields: &[&str]) -> String {
    let mut encoded = String::new();
    for field in fields {
        encoded.push_str(&field.len().to_string());
        encoded.push(':');
        encoded.push_str(field);
    }
    encoded
}

/// Domain-separated digest over a field list, rendered as `sha256:<hex>`.
#[must_use]
pub fn domain_digest(domain: &str, fields: &[&str]) -> String {
    let mut all: Vec<&str> = Vec::with_capacity(fields.len() + 1);
    all.push(domain);
    all.extend_from_slice(fields);
    format!("sha256:{}", sha256_hex(&length_prefixed(&all)))
}

/// Deterministic JSON with lexicographically sorted object keys.
///
/// Structurally equal payloads always serialize to identical bytes, so a
/// digest taken here is stable across key insertion order and across the two
/// language implementations.
#[must_use]
pub fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body = keys
                .iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String((*key).clone()),
                        canonical_json(&map[*key])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        serde_json::Value::Array(items) => {
            let body = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{body}]")
        }
        other => other.to_string(),
    }
}

/// Digest of the canonical serialization, prefixed with its algorithm.
#[must_use]
pub fn canonical_digest(value: &serde_json::Value) -> String {
    format!("sha256:{}", sha256_hex(&canonical_json(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_fips_vector() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn length_prefixing_separates_lists_a_delimiter_would_confuse() {
        // Both lists join to "a|b" under a naive separator scheme.
        let left = length_prefixed(&["a|b"]);
        let right = length_prefixed(&["a", "b"]);
        assert_ne!(left, right);
        assert_eq!(left, "3:a|b");
        assert_eq!(right, "1:a1:b");
    }

    #[test]
    fn length_prefixing_counts_utf8_bytes_not_chars() {
        // A 1-char, 4-byte field must not be readable as a 1-byte field.
        assert_eq!(length_prefixed(&["\u{1F600}"]), "4:\u{1F600}");
    }

    #[test]
    fn domains_keep_identical_field_lists_apart() {
        let fields = ["same"];
        assert_ne!(
            domain_digest(domain::CHUNK, &fields),
            domain_digest(domain::SOURCE, &fields)
        );
    }

    #[test]
    fn canonical_json_sorts_keys_and_ignores_insertion_order() {
        let left: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let right: serde_json::Value =
            serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&left), canonical_json(&right));
        assert_eq!(canonical_json(&left), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn canonical_json_escapes_keys_so_a_quote_cannot_forge_structure() {
        let value: serde_json::Value = serde_json::from_str(r#"{"a\"b":1}"#).unwrap();
        assert_eq!(canonical_json(&value), r#"{"a\"b":1}"#);
    }
}
