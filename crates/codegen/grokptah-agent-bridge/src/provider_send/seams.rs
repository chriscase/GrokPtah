//! Typed, versioned seams for authorities that are not on the assembled
//! mainline yet (#478).
//!
//! Each seam is a *narrow* value type, not a reimplementation. The send lattice
//! binds the seam's opaque generation identifier so that when the real
//! authority lands, only the mint path changes: the durable record shape, the
//! binding digest, and every consumer stay put.
//!
//! Every seam carries its own schema version. A durable attempt records the
//! version it was bound at, so a later authority can tell a provisional binding
//! from an authoritative one without guessing.

use serde::{Deserialize, Serialize};
use std::fmt;

use super::identity::{opaque_digest, OpaqueId};

/// How a seam value was obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeamProvenance {
    /// Minted by the landed authority for this seam.
    Authoritative,
    /// Derived locally because the authority is not on the mainline yet.
    /// Honest, stable, and re-derivable, but not a substitute for the real one.
    Provisional,
}

impl SeamProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authoritative => "authoritative",
            Self::Provisional => "provisional",
        }
    }
}

macro_rules! generation_seam {
    (
        $(#[$meta:meta])*
        $name:ident, $version_const:ident = $version:expr, $domain:literal
    ) => {
        /// Schema version of this seam. Bumped only when the bound shape
        /// changes, never when the upstream authority changes internally.
        pub const $version_const: u32 = $version;

        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            version: u32,
            provenance: SeamProvenance,
            /// Opaque, secret-free, re-derivable generation identifier.
            generation: OpaqueId,
        }

        impl $name {
            /// Bind a generation minted by the landed authority.
            pub fn authoritative(generation: OpaqueId) -> Self {
                Self {
                    version: $version_const,
                    provenance: SeamProvenance::Authoritative,
                    generation,
                }
            }

            /// Derive a stable provisional generation from non-secret inputs.
            ///
            /// The inputs are hashed, never stored, so a caller cannot leak a
            /// credential or a prompt into a durable record through this path.
            pub fn provisional(inputs: &[&str]) -> Self {
                Self {
                    version: $version_const,
                    provenance: SeamProvenance::Provisional,
                    generation: opaque_digest($domain, inputs),
                }
            }

            pub fn version(&self) -> u32 {
                self.version
            }

            pub fn provenance(&self) -> SeamProvenance {
                self.provenance
            }

            pub fn generation(&self) -> &OpaqueId {
                &self.generation
            }

            /// Canonical bytes contributed to the attempt binding digest.
            pub(crate) fn digest_input(&self) -> String {
                format!(
                    "{}:{}:{}:{}",
                    $domain,
                    self.version,
                    self.provenance.as_str(),
                    self.generation.as_str()
                )
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}@v{}", self.generation.as_str(), self.version)
            }
        }
    };
}

generation_seam!(
    /// #477 canonical principal / auth generation.
    ///
    /// The lattice never mints a principal; it binds the generation so that an
    /// attempt can be attributed to exactly one auth incarnation.
    PrincipalGeneration,
    PRINCIPAL_SEAM_VERSION = 1,
    "grokptah.provider_send.principal.v1"
);

generation_seam!(
    /// #458 capability / policy generation.
    ///
    /// Binds the capability set that admitted this send, so a later policy
    /// change cannot retroactively justify or condemn an attempt.
    CapabilityGeneration,
    CAPABILITY_SEAM_VERSION = 1,
    "grokptah.provider_send.capability.v1"
);

generation_seam!(
    /// #455 / #468 lifecycle authority generation.
    ///
    /// The lattice does not decide lifecycle; it records which lifecycle
    /// incarnation owned the attempt so a takeover can be attributed.
    LifecycleGeneration,
    LIFECYCLE_SEAM_VERSION = 1,
    "grokptah.provider_send.lifecycle.v1"
);

generation_seam!(
    /// #461 queue ownership generation.
    ///
    /// Records which queue-ownership incarnation admitted the work behind this
    /// send. Queue semantics stay entirely in #461.
    QueueOwnershipGeneration,
    QUEUE_OWNERSHIP_SEAM_VERSION = 1,
    "grokptah.provider_send.queue.v1"
);

generation_seam!(
    /// #462 canonical audit generation.
    ///
    /// The lattice emits an audit *outcome* inside the single atomic
    /// settlement; it does not own the audit log.
    AuditGeneration,
    AUDIT_SEAM_VERSION = 1,
    "grokptah.provider_send.audit.v1"
);

/// #466 reconciliation grant.
///
/// Resolving an `Uncertain` attempt is the *only* thing this grant permits, and
/// it may never perform provider I/O. The grant is a value, not a capability to
/// call anything: the lattice accepts a resolution accompanied by one, and the
/// reconciliation authority decides when to issue it.
pub const RECONCILIATION_SEAM_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationGrant {
    version: u32,
    provenance: SeamProvenance,
    grant_id: OpaqueId,
    /// What the operator explicitly authorized this grant to conclude.
    resolution: ReconciliationResolution,
}

/// The only two conclusions a reconciliation grant may carry. Neither is
/// derivable from provider I/O performed by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationResolution {
    /// Out-of-band evidence shows the provider did complete the request.
    ObservedDelivered,
    /// Out-of-band evidence shows the provider never accepted the request.
    ObservedNotDelivered,
}

impl ReconciliationGrant {
    pub fn authoritative(grant_id: OpaqueId, resolution: ReconciliationResolution) -> Self {
        Self {
            version: RECONCILIATION_SEAM_VERSION,
            provenance: SeamProvenance::Authoritative,
            grant_id,
            resolution,
        }
    }

    /// Provisional grants exist so the lattice can be exercised before #466
    /// lands. They are recorded as provisional and never silently promoted.
    pub fn provisional(inputs: &[&str], resolution: ReconciliationResolution) -> Self {
        Self {
            version: RECONCILIATION_SEAM_VERSION,
            provenance: SeamProvenance::Provisional,
            grant_id: opaque_digest("grokptah.provider_send.reconciliation.v1", inputs),
            resolution,
        }
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn provenance(&self) -> SeamProvenance {
        self.provenance
    }

    pub fn grant_id(&self) -> &OpaqueId {
        &self.grant_id
    }

    pub fn resolution(&self) -> ReconciliationResolution {
        self.resolution
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisional_generations_are_stable_and_do_not_echo_inputs() {
        let first = PrincipalGeneration::provisional(&["provider-a", "operator-1"]);
        let second = PrincipalGeneration::provisional(&["provider-a", "operator-1"]);
        assert_eq!(first, second);
        assert_eq!(first.provenance(), SeamProvenance::Provisional);
        assert!(!first.generation().as_str().contains("operator-1"));
        assert!(!first.generation().as_str().contains("provider-a"));
    }

    #[test]
    fn different_inputs_produce_different_generations() {
        let first = CapabilityGeneration::provisional(&["policy-1"]);
        let second = CapabilityGeneration::provisional(&["policy-2"]);
        assert_ne!(first.generation(), second.generation());
    }

    #[test]
    fn seams_are_domain_separated() {
        let principal = PrincipalGeneration::provisional(&["same"]);
        let capability = CapabilityGeneration::provisional(&["same"]);
        let lifecycle = LifecycleGeneration::provisional(&["same"]);
        let queue = QueueOwnershipGeneration::provisional(&["same"]);
        let audit = AuditGeneration::provisional(&["same"]);
        let all = [
            principal.generation().as_str().to_string(),
            capability.generation().as_str().to_string(),
            lifecycle.generation().as_str().to_string(),
            queue.generation().as_str().to_string(),
            audit.generation().as_str().to_string(),
        ];
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "seams must not collide");
    }

    #[test]
    fn authoritative_and_provisional_bind_differently() {
        let provisional = PrincipalGeneration::provisional(&["x"]);
        let authoritative = PrincipalGeneration::authoritative(provisional.generation().clone());
        assert_ne!(provisional.digest_input(), authoritative.digest_input());
    }
}
