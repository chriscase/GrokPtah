//! Opaque, deterministic identities and payload fingerprints.
//!
//! Run and lease identities are derived, never sequential: a consumer cannot
//! enumerate other runs by counting, and a host path or prompt never appears
//! inside an identity. The same derivation gives the idempotency ledger an
//! exact payload fingerprint so a replayed `request_id` with changed content
//! is refused instead of silently creating a second run.

/// FNV-1a over 128 bits. Deterministic, allocation-free, and dependency-free.
pub fn fingerprint(parts: &[&str]) -> String {
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013B;
    let mut hash = OFFSET_BASIS;
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hash ^= 0x1f;
            hash = hash.wrapping_mul(PRIME);
        }
        for byte in part.as_bytes() {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    format!("{hash:032x}")
}

/// Derive a prefixed opaque identity from exact inputs.
pub fn opaque_id(prefix: &str, parts: &[&str]) -> String {
    format!("{prefix}-{}", &fingerprint(parts)[..24])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_deterministic_and_input_sensitive() {
        assert_eq!(
            opaque_id("run", &["session-1", "req-1"]),
            opaque_id("run", &["session-1", "req-1"])
        );
        assert_ne!(
            opaque_id("run", &["session-1", "req-1"]),
            opaque_id("run", &["session-1", "req-2"])
        );
    }

    #[test]
    fn field_separation_prevents_boundary_collisions() {
        assert_ne!(fingerprint(&["ab", "c"]), fingerprint(&["a", "bc"]));
    }

    #[test]
    fn identities_never_echo_their_inputs() {
        let derived = opaque_id("run", &["/private/home", "sk-secret-value"]);
        assert!(!derived.contains("private"));
        assert!(!derived.contains("secret"));
        assert!(derived.starts_with("run-"));
        assert_eq!(derived.len(), 4 + 24);
    }
}
