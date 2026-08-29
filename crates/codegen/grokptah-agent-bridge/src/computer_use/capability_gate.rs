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
use crate::capability_authority::{
    CapabilityBindingRef, CapabilityBoundary, CapabilityDenied, DispatchEffect, DispatchLease,
};

/// Refusal used for every capability failure the kernel surfaces.
///
/// One code and one message, so a foreign, unknown, revoked or stale binding
/// is indistinguishable at the kernel boundary too. Nothing downstream of this
/// function may add discriminating context.
pub(crate) fn capability_denied() -> ComputerError {
    ComputerError::new(ComputerErrorCode::Unauthorized, CapabilityDenied::MESSAGE)
}

/// Who is driving one kernel boundary.
///
/// This is deliberately a named actor rather than `Option<&binding>`. Inferring
/// "no binding, therefore the operator" from an absence means any path that
/// drops a binding silently widens the run into the operator's authority. Here
/// the third case is spelled out and denies.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ComputerActor<'a> {
    /// The local operator, proven by the live one-use [`super::ActionGrant`]
    /// the run holds. That grant is the strongest operator proof this kernel
    /// has; a verified operator *identity* is separate work (#477/#455) and is
    /// not claimed here.
    Operator,
    /// A model, proven by a capability binding the live authority still
    /// honours.
    Model(&'a CapabilityBindingRef),
    /// A run that was being driven by a model and no longer carries the
    /// binding, while still holding the grant it was driving under. Its
    /// authority was stripped, not handed back, so it is nobody — and nobody
    /// dispatches.
    Stripped,
}

impl ComputerActor<'_> {
    /// Reads the actor off a run.
    ///
    /// The order matters: a live binding is a model, a matching stripped grant
    /// is nobody, and only a run that never carried model authority — or that
    /// has since been re-authorized by the operator with a fresh grant — is
    /// the operator.
    pub(crate) fn of(run: &super::ComputerRun) -> ComputerActor<'_> {
        if let Some(binding) = run.capability_binding.as_ref() {
            return ComputerActor::Model(binding);
        }
        let current_grant = run.grant.as_ref().map(|grant| grant.grant_id.as_str());
        match (run.model_authority_grant_id.as_deref(), current_grant) {
            (Some(stripped), Some(current)) if stripped == current => ComputerActor::Stripped,
            _ => ComputerActor::Operator,
        }
    }
}

/// Decides whether a model-attributed run may pass one kernel boundary.
///
/// Crate-internal, and deliberately not injectable from outside: a public
/// gate trait would let a caller install an allow-all boundary in production,
/// which is the one thing this seam exists to make impossible. The host is the
/// only implementor, and [`crate::AgentHostHandle::computer_use_service`] is
/// the only way to get a kernel wired to it.
pub(crate) trait ComputerCapabilityGate: Debug + Send + Sync {
    fn authorize(
        &self,
        boundary: CapabilityBoundary,
        owner_session_id: Uuid,
        actor: ComputerActor<'_>,
    ) -> Result<(), ComputerError>;

    /// Authorizes one exact physical effect and issues its single-use lease.
    ///
    /// `None` is an operator-driven run, which has nothing to redeem.
    fn authorize_dispatch(
        &self,
        owner_session_id: Uuid,
        actor: ComputerActor<'_>,
        effect: &DispatchEffect,
    ) -> Result<Option<DispatchLease>, ComputerError>;

    /// Consumes a lease against the effect that is about to happen.
    ///
    /// The capability is re-derived here, so the authorization is good only
    /// while the capability it was issued against is still exactly the same
    /// one. Between issue and redemption there is no window in which a
    /// downgrade could be missed: a moved capability produces a different
    /// digest and the lease no longer redeems.
    fn redeem_dispatch(
        &self,
        owner_session_id: Uuid,
        actor: ComputerActor<'_>,
        lease: Option<DispatchLease>,
        effect: &DispatchEffect,
    ) -> Result<(), ComputerError>;
}

/// The gate a kernel gets when nobody wired a provider authority to it.
///
/// It admits operator-driven runs, which need no provider capability at all,
/// and refuses every run that carries model authority. That is the fail-closed
/// direction: a kernel with no way to check a capability must not be the
/// kernel that dispatches on one.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct OperatorOnlyCapabilityGate;

impl ComputerCapabilityGate for OperatorOnlyCapabilityGate {
    fn authorize(
        &self,
        _boundary: CapabilityBoundary,
        _owner_session_id: Uuid,
        actor: ComputerActor<'_>,
    ) -> Result<(), ComputerError> {
        match actor {
            ComputerActor::Operator => Ok(()),
            ComputerActor::Model(_) | ComputerActor::Stripped => Err(capability_denied()),
        }
    }

    fn authorize_dispatch(
        &self,
        _owner_session_id: Uuid,
        actor: ComputerActor<'_>,
        _effect: &DispatchEffect,
    ) -> Result<Option<DispatchLease>, ComputerError> {
        match actor {
            ComputerActor::Operator => Ok(None),
            ComputerActor::Model(_) | ComputerActor::Stripped => Err(capability_denied()),
        }
    }

    fn redeem_dispatch(
        &self,
        _owner_session_id: Uuid,
        actor: ComputerActor<'_>,
        lease: Option<DispatchLease>,
        _effect: &DispatchEffect,
    ) -> Result<(), ComputerError> {
        match (actor, lease) {
            (ComputerActor::Operator, None) => Ok(()),
            _ => Err(capability_denied()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::computer_use::types::{
        ActionClass, ActionGrant, ComputerRun, ComputerTarget, ComputerUseLimits, GrantIssuer,
        Sensitivity,
    };
    use chrono::{Duration, Utc};
    use std::collections::BTreeSet;

    fn run_with_grant() -> ComputerRun {
        let mut run = ComputerRun::new(
            Uuid::new_v4(),
            None,
            ComputerTarget {
                app_id: "com.grokptah.actor-fixture".into(),
                window_id: "main".into(),
                generation: 1,
                display_name: "Actor fixture".into(),
                sensitivity: Sensitivity::None,
            },
            ComputerUseLimits::default(),
        )
        .expect("run");
        let issued_at = Utc::now() - Duration::seconds(1);
        run.grant = Some(ActionGrant {
            grant_id: "grant-1".into(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            action_classes: BTreeSet::from([ActionClass::Semantic]),
            issued_by: GrantIssuer::LocalUser,
            issued_at,
            expires_at: issued_at + Duration::minutes(5),
            uses_remaining: Some(4),
            revoked_at: None,
        });
        run
    }

    #[test]
    fn a_run_that_never_carried_model_authority_is_the_operator() {
        let run = run_with_grant();
        assert!(matches!(ComputerActor::of(&run), ComputerActor::Operator));
    }

    #[test]
    fn a_bound_run_is_the_model() {
        let mut run = run_with_grant();
        run.capability_binding = Some(CapabilityBindingRef::unbound());
        assert!(matches!(ComputerActor::of(&run), ComputerActor::Model(_)));
    }

    /// The widening this closes: a binding removed while the grant it was
    /// driving under is still live must not read as the operator.
    #[test]
    fn a_stripped_binding_is_nobody_and_dispatches_nothing() {
        let mut run = run_with_grant();
        run.model_authority_grant_id = Some("grant-1".into());
        run.capability_binding = None;
        assert!(matches!(ComputerActor::of(&run), ComputerActor::Stripped));

        let gate = OperatorOnlyCapabilityGate;
        let denied = gate
            .authorize(
                CapabilityBoundary::Dispatch,
                run.owner_session_id,
                ComputerActor::of(&run),
            )
            .expect_err("a stripped run dispatches nothing");
        assert_eq!(denied.message, CapabilityDenied::MESSAGE);
    }

    /// And the flow this must not break: handing control back revokes the
    /// grant, so a fresh operator grant makes the run honestly operator-driven
    /// again.
    #[test]
    fn a_fresh_operator_grant_returns_a_stripped_run_to_the_operator() {
        let mut run = run_with_grant();
        run.model_authority_grant_id = Some("grant-1".into());
        run.capability_binding = None;
        assert!(matches!(ComputerActor::of(&run), ComputerActor::Stripped));
        if let Some(grant) = run.grant.as_mut() {
            grant.grant_id = "grant-2".into();
        }
        assert!(
            matches!(ComputerActor::of(&run), ComputerActor::Operator),
            "take over and resume must still work"
        );
    }

    #[test]
    fn an_unwired_kernel_admits_the_operator_and_refuses_every_other_actor() {
        let gate = OperatorOnlyCapabilityGate;
        let session = Uuid::new_v4();
        let binding = CapabilityBindingRef::unbound();
        for boundary in CapabilityBoundary::ALL {
            gate.authorize(boundary, session, ComputerActor::Operator)
                .expect("operator-driven runs need no provider capability");
            for actor in [ComputerActor::Model(&binding), ComputerActor::Stripped] {
                let denied = gate
                    .authorize(boundary, session, actor)
                    .expect_err("only the operator passes an unwired kernel");
                assert_eq!(denied.code, ComputerErrorCode::Unauthorized);
                assert_eq!(denied.message, CapabilityDenied::MESSAGE);
                let effect = DispatchEffect::new("run", "observation", "semantic");
                assert!(gate.authorize_dispatch(session, actor, &effect).is_err());
            }
        }
    }
}
