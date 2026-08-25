//! Validated identifier newtypes.
//!
//! Distinct types keep a task identifier from silently standing in for a
//! worker or dispatch identifier at a call site. Every identifier is parsed
//! once, at the boundary, against a bounded conservative charset so that no
//! identifier can smuggle whitespace, control characters, or path separators
//! into a durable record or a public projection.

use serde::{Deserialize, Serialize};
use xai_grok_secrets::redact_secrets;

use crate::error::{SwarmError, SwarmResult};

/// Maximum bytes in any control-plane identifier.
pub const MAX_ID_BYTES: usize = 128;

/// True when `value` is a legal identifier body.
fn id_charset_ok(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

pub(crate) fn validate_id_str(value: &str, field: &str) -> SwarmResult<()> {
    if value.is_empty() {
        return Err(SwarmError::invalid(format!("{field} must not be empty")));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(SwarmError::invalid(format!(
            "{field} exceeds {MAX_ID_BYTES} bytes"
        )));
    }
    if !id_charset_ok(value) {
        return Err(SwarmError::invalid(format!(
            "{field} may only contain ASCII alphanumerics and '-', '_', '.', ':'"
        )));
    }
    if matches!(redact_secrets(value), std::borrow::Cow::Owned(_)) {
        return Err(SwarmError::invalid(format!(
            "{field} must not contain credential-shaped material"
        )));
    }
    Ok(())
}

macro_rules! swarm_id {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse and validate an identifier.
            pub fn parse(value: impl Into<String>) -> SwarmResult<Self> {
                let value = value.into();
                validate_id_str(&value, $field)?;
                Ok(Self(value))
            }

            /// Borrow the validated identifier body.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Re-check the invariant after a durable reload.
            pub(crate) fn validate(&self) -> SwarmResult<()> {
                validate_id_str(&self.0, $field)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

swarm_id!(SwarmId, "swarmId", "Identity of one swarm campaign.");
swarm_id!(TaskId, "taskId", "Identity of one node in the task graph.");
swarm_id!(
    WorkerId,
    "workerId",
    "Identity of one worker specification."
);
swarm_id!(
    DispatchId,
    "dispatchId",
    "Identity of one dispatch attempt for one task. Content-derived, so a \
     replay of the same attempt produces the same value."
);
swarm_id!(
    LeaseId,
    "leaseId",
    "Identity of an operator-issued Computer Use lease. The control plane \
     references leases; it never issues them."
);
swarm_id!(
    ExternalRefId,
    "externalRef",
    "Provider-side handle for a running child, recorded only once the \
     provider has acknowledged the dispatch."
);
swarm_id!(
    CredentialRef,
    "credentialRef",
    "Name of a credential held elsewhere (OS keychain or host config). This \
     is a reference only; the control plane never stores a secret value and \
     never emits this field in a public projection."
);

/// Provider family a worker is dispatched to (for example `grok`, `claude`,
/// `cursor`). The control plane is provider-neutral: it validates the
/// identifier's shape and its presence in the measured catalog, and attaches
/// no behavior to any particular name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(value: impl Into<String>) -> SwarmResult<Self> {
        let value = value.into();
        validate_id_str(&value, "provider")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn validate(&self) -> SwarmResult<()> {
        validate_id_str(&self.0, "provider")
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Exact model identifier as the provider spells it.
///
/// Model identifiers are preserved byte-for-byte — the repository's provider
/// profiles already require that returned catalog IDs are used exactly — so
/// this charset additionally permits `/` for namespaced gateway models.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn parse(value: impl Into<String>) -> SwarmResult<Self> {
        let value = value.into();
        Self::validate_str(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate_str(value: &str) -> SwarmResult<()> {
        if value.is_empty() {
            return Err(SwarmError::invalid("model must not be empty"));
        }
        if value.len() > MAX_ID_BYTES {
            return Err(SwarmError::invalid(format!(
                "model exceeds {MAX_ID_BYTES} bytes"
            )));
        }
        let ok = value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'));
        if !ok {
            return Err(SwarmError::invalid(
                "model may only contain ASCII alphanumerics and '-', '_', '.', ':', '/'",
            ));
        }
        if matches!(redact_secrets(value), std::borrow::Cow::Owned(_)) {
            return Err(SwarmError::invalid(
                "model must not contain credential-shaped material",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> SwarmResult<()> {
        Self::validate_str(&self.0)
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
