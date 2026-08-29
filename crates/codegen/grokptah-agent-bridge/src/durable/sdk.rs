//! The provider-neutral embeddable manager boundary.
//!
//! Two properties this module exists to hold:
//!
//! 1. **No raw transport.** Nothing here names an HTTP client, a URL, a header,
//!    a credential or a provider dialect. An embedder drives typed operations;
//!    it never gets a socket, and a provider swap is not a breaking change.
//! 2. **No self-asserted operator escape.** [`OperatorGrant`] has no public
//!    constructor, no public field, and is not reachable from
//!    [`ManagerSession::open`]. A session holding any set of [`Capability`]
//!    values still cannot elevate itself; it must be *handed* a grant. The one
//!    minting path in the public API is [`grant_operator_for_host`], which is a
//!    single greppable choke point rather than a struct literal anyone can
//!    write. That is what keeps "bearer plus operator authority is
//!    operator-equivalent" — the residual #471 records against itself — from
//!    being reachable through this seam.
//!
//! Canonical principal identity, auth generations and delegation belong to the
//! G1–G4 authority train. This module deliberately holds only a
//! [`GrantProvenance`] marker so that a provisional grant can never be mistaken
//! for a canonical one.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Versioned control-surface protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolVersion {
    V1,
    V2,
}

impl ProtocolVersion {
    /// Versions this build implements, oldest first.
    pub const SUPPORTED: &'static [ProtocolVersion] = &[ProtocolVersion::V1, ProtocolVersion::V2];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why negotiation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NegotiationError {
    /// The client offered nothing this build implements.
    NoCommonVersion { offered: Vec<String> },
    /// The client offered a version string this build does not recognise.
    ///
    /// Refused rather than ignored: silently dropping an unknown version is how
    /// a newer client ends up believing it negotiated a feature set the host
    /// does not have.
    UnknownVersion { name: String },
    /// The client offered nothing at all.
    Empty,
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCommonVersion { .. } => f.write_str("no mutually supported protocol version"),
            Self::UnknownVersion { name } => write!(f, "unsupported protocol version `{name}`"),
            Self::Empty => f.write_str("client offered no protocol version"),
        }
    }
}

impl std::error::Error for NegotiationError {}

/// Strict version negotiation.
///
/// Every offered name must be recognised; the highest mutually supported
/// version wins. An unrecognised name is an error even when another offer would
/// have succeeded, so a client cannot smuggle an unimplemented version past the
/// host by listing a known one beside it.
pub fn negotiate(offered: &[&str]) -> Result<ProtocolVersion, NegotiationError> {
    if offered.is_empty() {
        return Err(NegotiationError::Empty);
    }
    let mut parsed = Vec::with_capacity(offered.len());
    for name in offered {
        let version = ProtocolVersion::SUPPORTED
            .iter()
            .find(|v| v.as_str() == *name)
            .copied();
        match version {
            Some(v) => parsed.push(v),
            None => {
                return Err(NegotiationError::UnknownVersion {
                    name: (*name).to_string(),
                })
            }
        }
    }
    parsed
        .into_iter()
        .max()
        .ok_or_else(|| NegotiationError::NoCommonVersion {
            offered: offered.iter().map(|s| (*s).to_string()).collect(),
        })
}

/// What an embedded manager is allowed to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ReadRuns,
    SubmitWork,
    CancelWork,
    ReadAudit,
}

/// Where a grant came from.
///
/// A provisional grant is one this build minted from non-canonical inputs
/// because the canonical authority does not exist yet. Recording it keeps a
/// provisional identity from being read as a canonical one later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantProvenance {
    /// Issued by the canonical principal/auth authority (G1–G4).
    Canonical,
    /// Issued locally, pending that authority.
    Provisional,
}

/// Authority to act as the operator.
///
/// There is no public constructor and no public field. An embedder that wants
/// one must be given it by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorGrant {
    provenance: GrantProvenance,
}

impl OperatorGrant {
    /// Mint an operator grant. Crate-internal on purpose.
    pub(crate) fn issue(provenance: GrantProvenance) -> Self {
        Self { provenance }
    }

    pub fn provenance(&self) -> GrantProvenance {
        self.provenance
    }

    /// Whether this grant may be relied on as canonical authority.
    pub fn is_canonical(&self) -> bool {
        self.provenance == GrantProvenance::Canonical
    }
}

/// Issue operator authority for a manager session.
///
/// The single minting path in this crate's public API, and deliberately not a
/// method on [`ManagerSession`]: a session cannot elevate itself, it can only
/// be handed a grant by whatever code owns the host home.
///
/// `provenance` must be [`GrantProvenance::Canonical`] only when the caller
/// *is* the canonical principal/auth authority (G1-G4, #477/#460). Until that
/// authority exists, callers pass [`GrantProvenance::Provisional`] and every
/// consumer is expected to check [`OperatorGrant::is_canonical`] before relying
/// on it.
pub fn grant_operator_for_host(provenance: GrantProvenance) -> OperatorGrant {
    OperatorGrant::issue(provenance)
}

/// Refusals from the boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundaryError {
    /// The session holds no such capability.
    NotAuthorized,
    /// The operation needs operator authority the session was not granted.
    OperatorAuthorityRequired,
    /// The operation is not available at the negotiated protocol version.
    NotAvailableAtVersion { version: ProtocolVersion },
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Byte-identical for "no capability" and "no such object", so a
            // refusal is not an existence oracle.
            Self::NotAuthorized => f.write_str("not authorized"),
            Self::OperatorAuthorityRequired => f.write_str("not authorized"),
            Self::NotAvailableAtVersion { version } => {
                write!(f, "operation is not available at protocol {version}")
            }
        }
    }
}

impl std::error::Error for BoundaryError {}

/// An embedded manager's session against the host.
///
/// Provider-neutral: no method here takes or returns a URL, header, credential,
/// socket, or provider-specific value.
#[derive(Debug)]
pub struct ManagerSession {
    version: ProtocolVersion,
    capabilities: BTreeSet<Capability>,
    operator: Option<OperatorGrant>,
}

impl ManagerSession {
    /// Open a session for an embedder. Note that this cannot grant operator
    /// authority: the parameter list has nowhere to put one.
    pub fn open(
        version: ProtocolVersion,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        Self {
            version,
            capabilities: capabilities.into_iter().collect(),
            operator: None,
        }
    }

    /// Attach operator authority the host issued.
    pub fn with_operator(mut self, grant: OperatorGrant) -> Self {
        self.operator = Some(grant);
        self
    }

    pub fn version(&self) -> ProtocolVersion {
        self.version
    }

    pub fn has_operator_authority(&self) -> bool {
        self.operator.is_some()
    }

    /// Check one capability.
    pub fn require(&self, capability: Capability) -> Result<(), BoundaryError> {
        if self.capabilities.contains(&capability) {
            Ok(())
        } else {
            Err(BoundaryError::NotAuthorized)
        }
    }

    /// Check operator authority.
    pub fn require_operator(&self) -> Result<&OperatorGrant, BoundaryError> {
        self.operator
            .as_ref()
            .ok_or(BoundaryError::OperatorAuthorityRequired)
    }

    /// Reading the audit ledger arrived at v2; a v1 client cannot have it.
    pub fn require_version(&self, minimum: ProtocolVersion) -> Result<(), BoundaryError> {
        if self.version >= minimum {
            Ok(())
        } else {
            Err(BoundaryError::NotAvailableAtVersion {
                version: self.version,
            })
        }
    }
}

/// A redacted public view of one run.
///
/// Carries opaque handles and typed state only: no prompt, no response body, no
/// path, no credential, no provider route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProjection {
    /// Opaque handle. Not a path and not a provider id.
    pub handle: String,
    pub state: String,
    /// Honest delivery knowledge for the run's last provider attempt.
    pub delivery: super::send::DeliveryKnowledge,
    /// Structured stop detail when the run stopped repeating itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_detail: Option<super::progress::StopDetail>,
}
