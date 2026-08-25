//! Collision-resistant digests and hex encoding.
//!
//! Every identity in this crate — roots, documents, principals, policy — is a
//! BLAKE3 digest rather than a checksum. A 64-bit non-cryptographic hash is
//! adequate for a cache key and inadequate for anything a caller may rely on
//! to prove two things are the same file, which is precisely what the source
//! viewer asks its digests to do.

/// Domain-separated digest of a sequence of fields.
///
/// Fields are length-prefixed so `("ab", "c")` and `("a", "bc")` cannot
/// collide — the classic concatenation ambiguity.
pub fn tagged_digest(domain: &str, fields: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    for field in fields {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    *hasher.finalize().as_bytes()
}

/// Keyed digest, used for token authentication tags.
pub fn tagged_mac(key: &[u8; 32], domain: &str, fields: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    for field in fields {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    *hasher.finalize().as_bytes()
}

/// Lowercase hex, no separators.
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Parse lowercase or uppercase hex. Returns `None` on any non-hex byte or an
/// odd length, so a malformed token is refused rather than partially decoded.
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Constant-time equality for authentication tags.
///
/// Token verification must not leak how much of a forged tag was correct.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference: u8 = 0;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// A short, non-reversible display form of a digest.
///
/// Shown in the UI so a reader can tell two boundaries apart without the
/// absolute path ever crossing the process boundary.
pub fn digest_label(hex: &str) -> String {
    hex.chars().take(12).collect()
}
