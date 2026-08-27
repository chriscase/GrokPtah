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

/// Maximum UTF-8 bytes accepted in an [`ExternalRef`].
pub const MAX_EXTERNAL_REF_BYTES: usize = 128;

/// An opaque, bounded handle to a record this host does not own.
///
/// The headless host records *that* an orchestrator produced a provider
/// attempt and an operation receipt, and which ones, so an indeterminate
/// dispatch can later be reconciled. It deliberately does not restate what
/// those records mean: their contract, their state machine, and their
/// validation stay with the component that owns them. This type is the
/// carrier, not a second copy.
///
/// The accepted shape is deliberately conservative — printable ASCII
/// identifier characters, no whitespace, no markup, no traversal — so a
/// reference minted by the owning contract round-trips unchanged while
/// nothing unbounded or path-shaped reaches a durable record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ExternalRef(String);

impl ExternalRef {
    /// Accept a reference only when it is safe to record verbatim.
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.len() > MAX_EXTERNAL_REF_BYTES {
            return None;
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        }) {
            return None;
        }
        if value.contains("..") {
            return None;
        }
        let edge = |byte: u8| matches!(byte, b'/' | b'.' | b':' | b'-' | b'_');
        if value.bytes().next().is_some_and(edge) || value.bytes().next_back().is_some_and(edge) {
            return None;
        }
        Some(Self(value.to_owned()))
    }

    /// The bounded value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a decoded reference still satisfies its own bounds.
    ///
    /// A record read back from disk can contain anything the file held, so
    /// every read re-checks rather than trusting the type.
    pub fn is_bounded(&self) -> bool {
        Self::new(&self.0).as_ref() == Some(self)
    }
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
    fn external_refs_accept_owner_shaped_handles_and_refuse_the_rest() {
        for accepted in ["attempt-01HZ", "run/abc:1", "provider.request_id", "a"] {
            assert!(
                ExternalRef::new(accepted).is_some(),
                "{accepted} should be accepted"
            );
        }
        for refused in [
            "",
            "   ",
            "has space",
            "../escape",
            "/leading",
            "trailing-",
            "<script>",
            "new\nline",
        ] {
            assert!(
                ExternalRef::new(refused).is_none(),
                "{refused:?} should be refused"
            );
        }
        assert!(ExternalRef::new(&"x".repeat(MAX_EXTERNAL_REF_BYTES)).is_some());
        assert!(ExternalRef::new(&"x".repeat(MAX_EXTERNAL_REF_BYTES + 1)).is_none());
    }

    #[test]
    fn a_decoded_external_ref_is_re_checked_not_trusted() {
        let decoded: ExternalRef =
            serde_json::from_str("\"../smuggled\"").expect("serde is transparent");
        assert!(
            !decoded.is_bounded(),
            "a reference read back from disk must be re-validated"
        );
        let honest = ExternalRef::new("attempt-1").expect("bounded");
        assert!(honest.is_bounded());
        assert_eq!(
            serde_json::to_string(&honest).expect("serializes"),
            "\"attempt-1\""
        );
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
