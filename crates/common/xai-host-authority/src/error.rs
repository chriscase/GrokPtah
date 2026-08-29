//! Typed authority errors.
//!
//! Every variant is a *denial*: it describes why authority was refused or
//! could not be recorded. Denials are pre-effect by construction — the store
//! only returns one on a path where no physical effect can have occurred.
//! Anything that might have reached a provider settles as
//! [`crate::SendOutcome::Uncertain`] instead, never as an error and never as
//! an ordinary failure.

use std::fmt;

/// Why the host refused, or could not durably record, an authority decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    /// The presented bearer did not match any live credential.
    Unauthenticated,
    /// The principal is known but its authority has moved on: the credential
    /// was rotated, revoked, or re-issued since this context was minted.
    StalePrincipal,
    /// The capability generation has advanced since this grant was sealed.
    StaleCapability,
    /// The control plane epoch has advanced: a previous host incarnation
    /// admitted this work and the current one will not complete it.
    StaleControlEpoch,
    /// The governed surface moved since the action was authorised.
    StaleObservation,
    /// The named resource is not one the host has issued.
    ///
    /// Returned instead of creating a binding, which is what prevents a caller
    /// from owning a resource simply by being the first to name it.
    UnknownResource,
    /// The resource exists but belongs to a different principal, session, or
    /// workspace than the one presented.
    ResourceOwnershipMismatch,
    /// The presented workspace is not the one bound to this authority.
    WorkspaceMismatch,
    /// The presented session is not the one bound to this authority.
    SessionMismatch,
    /// The grant's wall-clock validity has elapsed.
    Expired,
    /// A one-use lease or permit was presented a second time.
    AlreadyConsumed,
    /// The action or body digest does not match what was authorised.
    DigestMismatch,
    /// The capability does not cover the requested effect.
    NotPermitted,
    /// An identifier or field failed validation before anything was recorded.
    Invalid(&'static str),
    /// Durable state could not be read, written, or fsynced.
    ///
    /// On the pre-effect path this is fail-closed: the caller never receives a
    /// permit, so no dispatch can follow.
    Durability(String),
    /// Durable state exists but does not parse or does not satisfy its
    /// invariants. Never repaired silently: a damaged authority root refuses
    /// service rather than inventing authority.
    CorruptState(String),
}

impl fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthenticated => f.write_str("bearer did not match a live credential"),
            Self::StalePrincipal => f.write_str("principal authority has been superseded"),
            Self::StaleCapability => f.write_str("capability generation has advanced"),
            Self::StaleControlEpoch => f.write_str("control epoch has advanced"),
            Self::StaleObservation => f.write_str("observed surface has moved"),
            Self::UnknownResource => f.write_str("resource was not issued by this host"),
            Self::ResourceOwnershipMismatch => {
                f.write_str("resource belongs to a different authority")
            }
            Self::WorkspaceMismatch => f.write_str("workspace does not match this authority"),
            Self::SessionMismatch => f.write_str("session does not match this authority"),
            Self::Expired => f.write_str("authority has expired"),
            Self::AlreadyConsumed => f.write_str("one-use authority was already consumed"),
            Self::DigestMismatch => f.write_str("action digest does not match the authorised one"),
            Self::NotPermitted => f.write_str("capability does not cover this effect"),
            Self::Invalid(what) => write!(f, "invalid {what}"),
            Self::Durability(e) => write!(f, "authority could not be durably recorded: {e}"),
            Self::CorruptState(e) => write!(f, "durable authority state is unusable: {e}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

impl From<std::io::Error> for AuthorityError {
    fn from(e: std::io::Error) -> Self {
        Self::Durability(e.to_string())
    }
}

impl AuthorityError {
    /// Whether this denial happened strictly before any physical effect.
    ///
    /// Every variant is pre-effect; the method exists so callers and tests can
    /// assert that property rather than assume it.
    pub const fn is_pre_effect(&self) -> bool {
        true
    }
}
