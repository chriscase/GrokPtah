//! Contract version identity and negotiation.
//!
//! This version describes the **capability boundary in this crate**, not the
//! MCP transport `protocolVersion` and not the runtime crate version. A host
//! may change its transport or its internal runtime freely; what a consumer
//! compiles against is the pair (`ContractVersion`, [`CapabilityDocument`]).
//!
//! [`CapabilityDocument`]: crate::capability::CapabilityDocument

use serde::{Deserialize, Serialize};

use crate::error::{SdkError, SdkErrorCode};

/// The contract version this build of the SDK speaks.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion { major: 1, minor: 0 };

/// Major.minor identity for the capability boundary.
///
/// * A **major** bump removes or reshapes an existing field, method, error
///   code, or capability identifier. Consumers must be recompiled.
/// * A **minor** bump is additive only: new optional fields, new capability
///   identifiers, new error codes. An older consumer keeps working because
///   unknown members decode into the forward-compatible variants
///   ([`SdkErrorCode::Unknown`], [`CapabilityId::Unknown`]) rather than
///   failing to parse.
///
/// [`CapabilityId::Unknown`]: crate::capability::CapabilityId::Unknown
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractVersion {
    pub major: u32,
    pub minor: u32,
}

impl ContractVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Same major family. Compatibility is never inferred from minor alone.
    pub const fn same_major(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Result of negotiating this build against a host's advertised version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Negotiated {
    /// What the consumer compiled against.
    pub consumer: ContractVersion,
    /// What the host advertised.
    pub host: ContractVersion,
    /// The minor level both sides can rely on: `min(consumer, host)`.
    pub effective: ContractVersion,
    /// `true` when the consumer is newer than the host, so consumer-side
    /// features above `effective.minor` must stay switched off. This is not an
    /// error; it is the additive-minor contract working as designed.
    pub degraded: bool,
}

/// Negotiate `consumer` against `host`.
///
/// A major mismatch is a hard, typed failure in **both** directions: an older
/// host cannot satisfy a newer major, and a newer host has reshaped fields the
/// older consumer would silently misread. Guessing is worse than refusing.
pub fn negotiate(consumer: ContractVersion, host: ContractVersion) -> Result<Negotiated, SdkError> {
    if !consumer.same_major(host) {
        return Err(SdkError::new(
            SdkErrorCode::ContractVersionUnsupported,
            format!("contract major mismatch: consumer speaks {consumer}, host advertises {host}"),
        )
        .with_detail("consumerContractVersion", consumer.to_string())
        .with_detail("hostContractVersion", host.to_string()));
    }
    let effective = ContractVersion::new(consumer.major, consumer.minor.min(host.minor));
    Ok(Negotiated {
        consumer,
        host,
        effective,
        degraded: consumer.minor > host.minor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_versions_are_not_degraded() {
        let n = negotiate(CONTRACT_VERSION, CONTRACT_VERSION).unwrap();
        assert_eq!(n.effective, CONTRACT_VERSION);
        assert!(!n.degraded);
    }

    #[test]
    fn older_host_minor_degrades_without_error() {
        let n = negotiate(ContractVersion::new(1, 4), ContractVersion::new(1, 1)).unwrap();
        assert_eq!(n.effective, ContractVersion::new(1, 1));
        assert!(n.degraded);
    }

    #[test]
    fn newer_host_minor_is_compatible_and_not_degraded() {
        let n = negotiate(ContractVersion::new(1, 1), ContractVersion::new(1, 7)).unwrap();
        assert_eq!(n.effective, ContractVersion::new(1, 1));
        assert!(!n.degraded);
    }

    #[test]
    fn major_mismatch_fails_closed_in_both_directions() {
        for (c, h) in [
            (ContractVersion::new(1, 0), ContractVersion::new(2, 0)),
            (ContractVersion::new(2, 0), ContractVersion::new(1, 9)),
        ] {
            let err = negotiate(c, h).unwrap_err();
            assert_eq!(err.code, SdkErrorCode::ContractVersionUnsupported);
        }
    }
}
