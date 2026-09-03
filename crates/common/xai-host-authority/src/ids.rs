//! Opaque, host-issued identity and generation values.
//!
//! Every type in this module has private fields and no public constructor. A
//! caller outside this crate therefore cannot mint one with a struct literal,
//! a tuple call, or `Default`; the only way to obtain one is to be handed it by
//! the [`crate::HostAuthority`] store after the host has actually issued it.
//! `tests/ui` pins that property with compile-fail cases.
//!
//! Their `Debug` and public projections are deliberately opaque: they render a
//! short, stable, non-reversible handle instead of the underlying bytes so that
//! logs, telemetry, and MCP projections never carry authority material.

use crate::digest::{hex, short_handle};

/// Declare an opaque 16-byte host-issued identifier.
macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident, $label:literal) => {
        $(#[$meta])*
        // Deliberately not `Serialize`/`Deserialize`. A derived `Deserialize`
        // is a public constructor in disguise: downstream code could mint an
        // identity straight from JSON and walk past the private constructor
        // these types rely on. Durable records carry hex strings instead, and
        // only this crate turns one back into an identifier.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Mint a fresh value. Crate-private: only the host issues identity.
            ///
            /// Generated uniformly for every identifier; a few are derived
            /// deterministically instead (a workspace from its canonical path)
            /// and so never call it.
            #[allow(dead_code)]
            pub(crate) fn mint() -> Self {
                Self(*uuid::Uuid::new_v4().as_bytes())
            }

            /// Rebuild from durable bytes during recovery. Crate-private.
            pub(crate) fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Full hex form, used only for durable keys inside this crate.
            pub(crate) fn to_hex(self) -> String {
                hex(&self.0)
            }

            /// Public, non-reversible handle safe for projections and logs.
            ///
            /// This is a truncated digest of the identifier, not the
            /// identifier itself, so publishing it cannot re-create authority.
            pub fn public_handle(&self) -> String {
                short_handle($label, &self.0)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // Never render the raw bytes.
                write!(f, "{}({})", stringify!($name), self.public_handle())
            }
        }
    };
}

opaque_id!(
    /// Host-issued identity of an authenticated principal.
    ///
    /// Stable across credential rotation: rotating a secret advances the
    /// principal's [`CredentialIncarnation`] and [`AuthGeneration`] but keeps
    /// this identity, so audit history stays attributable.
    PrincipalId,
    "prn"
);

opaque_id!(
    /// The incarnation of a principal's credential material.
    ///
    /// Any change to the underlying secret — rotation, removal and re-add,
    /// or an owner change — mints a fresh incarnation. A bearer captured
    /// under an older incarnation can never be resurrected against the new
    /// one, because the incarnation is part of every authority binding.
    CredentialIncarnation,
    "inc"
);

opaque_id!(
    /// Host-issued incarnation of a governed resource (a session, a workspace
    /// lease, a Computer Use surface).
    ///
    /// Issued by the host when it creates the resource. Callers never name a
    /// resource into existence, which is what closes the caller-first-claim
    /// hole: an unclaimed resource key is not claimable, it is simply unknown.
    ResourceIncarnation,
    "res"
);

opaque_id!(
    /// Identity of a single sealed capability grant.
    CapabilityId,
    "cap"
);

opaque_id!(
    /// Identity of a single one-use effect lease.
    EffectLeaseId,
    "lea"
);

opaque_id!(
    /// Identity of one provider physical-send attempt.
    AttemptId,
    "att"
);

opaque_id!(
    /// Identity of a host-issued session.
    SessionId,
    "ses"
);

opaque_id!(
    /// Identity of a host-issued workspace binding.
    WorkspaceId,
    "wsp"
);

/// Declare a monotonic, host-advanced generation counter.
macro_rules! generation {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        // Not `Deserialize`, for the same reason: a caller could otherwise
        // claim any generation it liked by parsing a number.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            pub(crate) fn from_raw(value: u64) -> Self {
                Self(value)
            }

            pub(crate) fn raw(self) -> u64 {
                self.0
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

generation!(
    /// Advances whenever a principal's authentication material changes.
    AuthGeneration
);

generation!(
    /// Advances whenever the capability policy is rotated or revoked.
    CapabilityGeneration
);

generation!(
    /// Advances whenever workspace allowlists, queue ownership policy, or other
    /// host policy that is distinct from credential rotation changes.
    ///
    /// This is not a second authentication epoch: credential rotation advances
    /// [`AuthGeneration`], while policy rotation advances this counter.
    PolicyRevision
);

generation!(
    /// Advances whenever the host's control plane restarts or re-arms.
    ///
    /// Binding effects to the control epoch means work admitted by a previous
    /// host incarnation cannot complete against the current one.
    ControlEpoch
);

generation!(
    /// Advances on every accepted observation of a governed surface.
    ///
    /// An action authorised against revision *n* cannot be applied once the
    /// surface has moved to *n+1*.
    ObservationRevision
);
