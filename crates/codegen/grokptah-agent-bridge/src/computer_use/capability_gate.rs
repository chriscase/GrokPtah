//! The seam the provider-neutral kernel checks model authority through.
//!
//! The kernel deliberately knows nothing about providers, routes, or
//! credentials. It does know whether a run is being driven by a model, because
//! a model-driven run carries a [`CapabilityBindingRef`] — and it knows that
//! such a run must not reach a screen unless something that *does* understand
//! providers says the capability behind it is still current.
//!
//! That is this trait. The kernel calls it at the lease, live-frame and
//! dispatch boundaries; the host implements it against the live capability
//! authority.

use std::fmt::Debug;

use uuid::Uuid;

use super::types::{ComputerError, ComputerErrorCode};
use crate::capability_authority::{CapabilityBindingRef, CapabilityBoundary, CapabilityDenied};

/// Refusal used for every capability failure the kernel surfaces.
///
/// One code and one message, so a foreign, unknown, revoked or stale binding
/// is indistinguishable at the kernel boundary too. Nothing downstream of this
/// function may add discriminating context.
pub fn capability_denied() -> ComputerError {
    ComputerError::new(ComputerErrorCode::Unauthorized, CapabilityDenied::MESSAGE)
}

/// Decides whether a model-attributed run may pass one kernel boundary.
pub trait ComputerCapabilityGate: Debug + Send + Sync {
    fn authorize(
        &self,
        boundary: CapabilityBoundary,
        owner_session_id: Uuid,
        binding: Option<&CapabilityBindingRef>,
    ) -> Result<(), ComputerError>;
}

/// The gate a kernel gets when nobody wired a provider authority to it.
///
/// It admits operator-driven runs, which need no provider capability at all,
/// and refuses every run that carries model authority. That is the fail-closed
/// direction: a kernel with no way to check a capability must not be the
/// kernel that dispatches on one.
#[derive(Debug, Default, Clone, Copy)]
pub struct OperatorOnlyCapabilityGate;

impl ComputerCapabilityGate for OperatorOnlyCapabilityGate {
    fn authorize(
        &self,
        _boundary: CapabilityBoundary,
        _owner_session_id: Uuid,
        binding: Option<&CapabilityBindingRef>,
    ) -> Result<(), ComputerError> {
        match binding {
            None => Ok(()),
            Some(_) => Err(capability_denied()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_authority::CapabilityDigest;

    fn reference() -> CapabilityBindingRef {
        CapabilityBindingRef {
            binding_id: Uuid::new_v4().to_string(),
            digest: serde_json::from_str::<CapabilityDigest>("\"v1-sha256:abc\"").expect("digest"),
            generation: 1,
        }
    }

    #[test]
    fn an_unwired_kernel_admits_the_operator_and_refuses_model_authority() {
        let gate = OperatorOnlyCapabilityGate;
        let session = Uuid::new_v4();
        for boundary in CapabilityBoundary::ALL {
            gate.authorize(boundary, session, None)
                .expect("operator-driven runs need no provider capability");
            let denied = gate
                .authorize(boundary, session, Some(&reference()))
                .expect_err("model authority must not pass an unwired kernel");
            assert_eq!(denied.code, ComputerErrorCode::Unauthorized);
            assert_eq!(denied.message, CapabilityDenied::MESSAGE);
        }
    }
}
