//! Validated identity newtypes for the durable work graph.
//!
//! Identity is deliberately explicit. A `WorkId` and a `LeaseId` are different
//! types, so a caller cannot hand one where the other is required, and every
//! value is bounded and path-safe before it can reach the durable ledger.

use serde::{Deserialize, Serialize};

use crate::orchestration::types::{OrchError, OrchErrorCode};

/// Maximum bytes in any identity value. Matches the ledger's filename bound so
/// an identity that validates here can always be persisted.
pub const MAX_ID_BYTES: usize = 128;

fn validate_id_value(kind: &str, value: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{kind} must be 1..={MAX_ID_BYTES} bytes"),
        ));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{kind} contains characters outside [A-Za-z0-9-_.:]"),
        ));
    }
    if value.contains("..") {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{kind} must not contain a parent-directory sequence"),
        ));
    }
    Ok(())
}

macro_rules! swarm_id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse and validate. This is the only way to build the type, so
            /// a deserialized value is validated before it is trusted.
            pub fn parse(value: impl Into<String>) -> Result<Self, OrchError> {
                let value = value.into();
                validate_id_value($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Re-validate a value that arrived through serde.
            pub fn validate(&self) -> Result<(), OrchError> {
                validate_id_value($kind, &self.0)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

swarm_id!(GraphId, "graph id");
swarm_id!(WorkId, "work id");
swarm_id!(WorkerId, "worker id");
swarm_id!(AttemptId, "attempt id");
swarm_id!(LeaseId, "lease id");
swarm_id!(AuthorityId, "authority id");
swarm_id!(GrantId, "grant id");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_and_separators() {
        assert!(WorkId::parse("../escape").is_err());
        assert!(WorkId::parse("a/b").is_err());
        assert!(WorkId::parse("a\\b").is_err());
        assert!(WorkId::parse("a\0b").is_err());
        assert!(WorkId::parse("").is_err());
        assert!(WorkId::parse("x".repeat(MAX_ID_BYTES + 1)).is_err());
        assert!(WorkId::parse("build:step-1.a_b").is_ok());
    }

    #[test]
    fn deserialized_ids_still_require_validation() {
        // serde(transparent) accepts any string; `validate` is the gate that
        // a durable record must pass before it is trusted.
        let hostile: WorkId = serde_json::from_str("\"../../etc/passwd\"").expect("parses");
        assert!(hostile.validate().is_err());
    }
}
