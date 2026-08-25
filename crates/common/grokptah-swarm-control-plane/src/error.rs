//! Fail-closed error taxonomy for the swarm control plane.
//!
//! Every rejection carries a machine-readable [`SwarmErrorCode`] plus a short
//! operator-facing message. Messages are authored by this crate and never
//! interpolate worker output, provider payloads, or credential material, so an
//! error is always safe to surface in a public projection.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable machine-readable reason a control-plane operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmErrorCode {
    /// A specification, graph, or policy failed deterministic validation.
    InvalidSpec,
    /// A worker names a provider, model, or capability the catalog does not
    /// measure as available. Never inferred from a name.
    CapabilityNotGranted,
    /// The operation targets an identifier that does not exist in this swarm.
    NotFound,
    /// The operation is not legal from the current lifecycle state.
    Conflict,
    /// An admission, concurrency, fan-out, or budget bound would be exceeded.
    BoundExceeded,
    /// The recorded dispatch outcome is unknown, so the work must not be
    /// resent without external evidence.
    UncertainDispatch,
    /// A durable record failed its invariants while being reloaded.
    CorruptState,
}

impl SwarmErrorCode {
    /// Canonical wire string (matches the serde `snake_case` representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid_spec",
            Self::CapabilityNotGranted => "capability_not_granted",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::BoundExceeded => "bound_exceeded",
            Self::UncertainDispatch => "uncertain_dispatch",
            Self::CorruptState => "corrupt_state",
        }
    }
}

/// A refused control-plane operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmError {
    pub code: SwarmErrorCode,
    pub message: String,
}

impl SwarmError {
    pub fn new(code: SwarmErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(SwarmErrorCode::InvalidSpec, message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(SwarmErrorCode::Conflict, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(SwarmErrorCode::NotFound, message)
    }

    pub(crate) fn bound(message: impl Into<String>) -> Self {
        Self::new(SwarmErrorCode::BoundExceeded, message)
    }

    pub(crate) fn capability(message: impl Into<String>) -> Self {
        Self::new(SwarmErrorCode::CapabilityNotGranted, message)
    }
}

impl fmt::Display for SwarmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for SwarmError {}

/// Result alias used throughout the control plane.
pub type SwarmResult<T> = Result<T, SwarmError>;
