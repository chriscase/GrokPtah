//! The adapter boundary between this host and an agent-loop orchestrator.
//!
//! The host owns lifecycle, durability, authority, and projection. Deciding
//! what to say to a provider, saying it, and recording what came back belong to
//! the orchestration that already does that work. This module is the seam
//! between the two, and it is deliberately one-directional: the host defines a
//! port, an orchestrator implements it, and nothing here reaches into provider
//! code.
//!
//! # What this is not
//!
//! It is not a second runtime: [`TurnOrchestrator`] is synchronous, so an
//! implementation drives its own executor and this crate creates none. It is
//! not a second send machine: nothing here dispatches anything, and the host
//! never decides that a request may be repeated. It is not a second authority
//! or identity model: the orchestrator states the session and workspace it is
//! bound to, and the adapter's only authority decision is to refuse when that
//! binding disagrees with the run.
//!
//! # Referenced, not restated
//!
//! An orchestrator that talks to a provider already records what was bound,
//! what was presented, and how far the answer got. That contract stays where it
//! is. A [`TurnReceipt`] carries only opaque [`ExternalRef`] handles to those
//! records plus the one classification the host must act on — whether this run
//! may move. Coarsening delivery state into
//! [`DispatchDisposition`](crate::engine::DispatchDisposition) is a projection
//! for the host's own decision, not a competing state machine, and its safe
//! default is "cannot tell".

use grokptah_agent_sdk::RunScope;

use crate::attention::AttentionKind;
use crate::engine::{
    DispatchDisposition, DispatchReport, EngineOutcome, EngineStep, RunEngine, StepResult,
};
use crate::identity::ExternalRef;
use crate::lifecycle::CancelSignal;

/// The exact session and workspace an orchestrator is bound to.
///
/// The workspace is the host's share-safe workspace alias, never a path: the
/// binding has to be comparable across a process boundary without either side
/// publishing a host path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorBinding {
    /// Session identity this orchestrator acts for.
    pub session: ExternalRef,
    /// Workspace alias this orchestrator acts within.
    pub workspace: ExternalRef,
}

impl OrchestratorBinding {
    /// Build a binding, refusing identifiers that are not bounded and opaque.
    pub fn new(session: &str, workspace: &str) -> Option<Self> {
        Some(Self {
            session: ExternalRef::new(session)?,
            workspace: ExternalRef::new(workspace)?,
        })
    }

    /// Whether this binding is the exact one a run scope requires.
    pub fn matches(&self, scope: &RunScope) -> bool {
        self.session.is_bounded()
            && self.workspace.is_bounded()
            && self.session.as_str() == scope.session_id
            && self.workspace.as_str() == scope.workspace
    }
}

/// What one orchestrated turn produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnReceipt {
    /// What the run produced.
    pub outcome: EngineOutcome,
    /// Whether the run may move, given what this turn established.
    pub disposition: DispatchDisposition,
    /// Opaque reference to the attempt record the orchestrator wrote.
    pub attempt: Option<ExternalRef>,
    /// Opaque reference to the operation receipt the orchestrator wrote.
    pub receipt: Option<ExternalRef>,
}

impl TurnReceipt {
    /// A turn that reached nothing outside this host.
    pub fn local(outcome: EngineOutcome) -> Self {
        Self {
            outcome,
            disposition: DispatchDisposition::Local,
            attempt: None,
            receipt: None,
        }
    }

    /// A turn that dispatched, with the references needed to reconcile it.
    pub fn dispatched(
        outcome: EngineOutcome,
        disposition: DispatchDisposition,
        attempt: Option<ExternalRef>,
        receipt: Option<ExternalRef>,
    ) -> Self {
        Self {
            outcome,
            disposition,
            attempt,
            receipt,
        }
    }
}

/// Why an orchestrator declined to run a turn at all.
///
/// Both arms mean nothing was dispatched — that is what makes them a refusal
/// rather than a receipt — but they differ in whether waiting could help, and
/// the host treats them differently because of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnRefusal {
    /// Nothing is wired up: no provider, profile, or credential route exists
    /// for this host. Waiting will not change that, so the run fails.
    NotConfigured {
        /// Stable machine-readable reason.
        reason_code: String,
        /// Operator-facing detail, redacted by the host before it is recorded.
        detail: String,
    },
    /// Configured, but not usable right now — offline, rate limited, a breaker
    /// open, no capacity. An operator can fix it, so the run halts and asks.
    Unavailable {
        /// Stable machine-readable reason.
        reason_code: String,
        /// Operator-facing detail, redacted by the host before it is recorded.
        detail: String,
    },
}

impl TurnRefusal {
    /// Stable label for events.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotConfigured { .. } => "not_configured",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// One bounded turn the host asks an orchestrator to run.
#[derive(Debug, Clone, Copy)]
pub struct TurnRequest<'a> {
    /// Exact run identity.
    pub scope: &'a RunScope,
    /// One-based round within the run's admitted bounds.
    pub round: u16,
    /// The admitted prompt.
    pub prompt: &'a str,
    /// Steering directives accepted since the last turn, oldest first.
    pub steering: &'a [String],
    /// Cooperative cancellation. An orchestrator that can block should check it.
    pub cancel: &'a CancelSignal,
    /// One-based dispatch ordinal, already durable before this call.
    ///
    /// Stable across the life of the turn and unique within the run, so an
    /// orchestrator can derive its own idempotency from it and match the record
    /// the host will hold if this process dies mid-turn.
    pub dispatch_ordinal: u32,
}

/// The port an agent-loop orchestrator implements to drive headless runs.
///
/// Synchronous on purpose. An orchestrator that is internally asynchronous
/// blocks on its own executor inside [`run_turn`](Self::run_turn); the host
/// starts no runtime of its own and holds no lock while the call is in flight.
pub trait TurnOrchestrator: Send {
    /// Stable label reported by health.
    fn label(&self) -> &'static str;

    /// The exact session and workspace this orchestrator acts for.
    fn binding(&self) -> OrchestratorBinding;

    /// Run one bounded turn, or refuse it.
    fn run_turn(&mut self, request: &TurnRequest<'_>) -> Result<TurnReceipt, TurnRefusal>;
}

/// Adapts a [`TurnOrchestrator`] to the host's [`RunEngine`] port.
///
/// The adapter adds exactly three things and nothing else: it refuses a turn
/// whose scope disagrees with the orchestrator's binding, it declines to
/// dispatch once cancellation has been requested, and it translates a refusal
/// into the host's own outcome vocabulary. Everything else passes through.
#[derive(Debug)]
pub struct OrchestratedEngine<T> {
    orchestrator: T,
}

impl<T: TurnOrchestrator> OrchestratedEngine<T> {
    /// Wrap an orchestrator as a run engine.
    pub fn new(orchestrator: T) -> Self {
        Self { orchestrator }
    }

    /// The wrapped orchestrator.
    pub fn orchestrator(&self) -> &T {
        &self.orchestrator
    }
}

impl<T: TurnOrchestrator> RunEngine for OrchestratedEngine<T> {
    fn label(&self) -> &'static str {
        "orchestrated"
    }

    fn step(&mut self, step: &EngineStep<'_>) -> StepResult {
        // Binding first, before anything is prepared. A mismatch means the
        // orchestrator would act for a session or workspace this run was never
        // admitted against, which is a refusal rather than a failure to report
        // after the fact.
        let binding = self.orchestrator.binding();
        if !binding.matches(step.scope) {
            return StepResult::local(EngineOutcome::Failed {
                reason_code: "orchestrator_binding_mismatch".to_owned(),
                detail: "the orchestrator is bound to a different session or workspace".to_owned(),
            });
        }

        // Nothing is dispatched once a stop has been requested. Halting rather
        // than failing keeps the run recoverable: the work was never started,
        // so there is nothing to reconcile and nothing to throw away.
        if step.cancel.is_cancelled() {
            return StepResult::local(EngineOutcome::NeedsAttention {
                attention: AttentionKind::RecoveryRequired,
                reason_code: "cancelled_before_dispatch".to_owned(),
                detail: "the host was stopping, so this turn was not dispatched".to_owned(),
            });
        }

        let request = TurnRequest {
            scope: step.scope,
            round: step.round,
            prompt: step.prompt,
            steering: step.steering,
            cancel: step.cancel,
            dispatch_ordinal: step.dispatch_ordinal,
        };

        match self.orchestrator.run_turn(&request) {
            Ok(receipt) => StepResult {
                outcome: receipt.outcome,
                dispatch: DispatchReport::external(
                    receipt.disposition,
                    receipt.attempt,
                    receipt.receipt,
                ),
            },
            Err(TurnRefusal::NotConfigured {
                reason_code,
                detail,
            }) => StepResult::local(EngineOutcome::Failed {
                reason_code,
                detail,
            }),
            Err(TurnRefusal::Unavailable {
                reason_code,
                detail,
            }) => StepResult::local(EngineOutcome::NeedsAttention {
                attention: AttentionKind::EngineFailure,
                reason_code,
                detail,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{DispatchLog, FakeOrchestrator, FakeTurn};

    fn scope(session: &str, workspace: &str) -> RunScope {
        RunScope {
            session_id: session.to_owned(),
            workspace: workspace.to_owned(),
            run_id: "run-1".to_owned(),
        }
    }

    fn take(engine: &mut OrchestratedEngine<FakeOrchestrator>, scope: &RunScope) -> StepResult {
        let cancel = CancelSignal::new();
        take_with(engine, scope, &cancel)
    }

    fn take_with(
        engine: &mut OrchestratedEngine<FakeOrchestrator>,
        scope: &RunScope,
        cancel: &CancelSignal,
    ) -> StepResult {
        engine.step(&EngineStep {
            scope,
            round: 1,
            prompt: "build",
            steering: &[],
            cancel,
            dispatch_ordinal: 1,
        })
    }

    fn engine(turns: Vec<FakeTurn>) -> OrchestratedEngine<FakeOrchestrator> {
        OrchestratedEngine::new(FakeOrchestrator::new(
            OrchestratorBinding::new("session-fixture", "project").expect("binding"),
            DispatchLog::new(),
            turns,
        ))
    }

    #[test]
    fn a_matching_binding_passes_the_receipt_through_unchanged() {
        let mut adapter = engine(vec![FakeTurn::dispatched(
            EngineOutcome::Progress {
                update: serde_json::json!({"note": "working"}),
            },
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            Some("receipt-1"),
        )]);
        let result = take(&mut adapter, &scope("session-fixture", "project"));
        assert_eq!(result.dispatch.disposition, DispatchDisposition::Resolved);
        assert_eq!(
            result.dispatch.attempt.as_ref().map(ExternalRef::as_str),
            Some("attempt-1")
        );
        assert!(matches!(result.outcome, EngineOutcome::Progress { .. }));
        assert_eq!(adapter.label(), "orchestrated");
    }

    #[test]
    fn a_mismatched_session_or_workspace_never_reaches_the_orchestrator() {
        for wrong in [
            scope("session-other", "project"),
            scope("session-fixture", "other-project"),
        ] {
            let log = DispatchLog::new();
            let mut adapter = OrchestratedEngine::new(FakeOrchestrator::new(
                OrchestratorBinding::new("session-fixture", "project").expect("binding"),
                log.clone(),
                vec![FakeTurn::dispatched(
                    EngineOutcome::Progress {
                        update: serde_json::json!({}),
                    },
                    DispatchDisposition::Resolved,
                    Some("attempt-1"),
                    None,
                )],
            ));
            let result = take(&mut adapter, &wrong);
            assert!(matches!(
                result.outcome,
                EngineOutcome::Failed { ref reason_code, .. }
                    if reason_code == "orchestrator_binding_mismatch"
            ));
            assert_eq!(result.dispatch.disposition, DispatchDisposition::Local);
            assert!(
                log.entries().is_empty(),
                "a mismatched binding must not dispatch"
            );
        }
    }

    #[test]
    fn cancellation_stops_the_turn_before_anything_is_dispatched() {
        let log = DispatchLog::new();
        let mut adapter = OrchestratedEngine::new(FakeOrchestrator::new(
            OrchestratorBinding::new("session-fixture", "project").expect("binding"),
            log.clone(),
            vec![FakeTurn::dispatched(
                EngineOutcome::Progress {
                    update: serde_json::json!({}),
                },
                DispatchDisposition::Resolved,
                None,
                None,
            )],
        ));
        let cancel = CancelSignal::new();
        cancel.cancel();

        let result = take_with(&mut adapter, &scope("session-fixture", "project"), &cancel);
        assert!(matches!(
            result.outcome,
            EngineOutcome::NeedsAttention { ref reason_code, .. }
                if reason_code == "cancelled_before_dispatch"
        ));
        assert_eq!(result.dispatch.disposition, DispatchDisposition::Local);
        assert!(log.entries().is_empty(), "cancellation must not dispatch");
    }

    #[test]
    fn refusals_map_to_terminal_or_recoverable_by_whether_waiting_helps() {
        let mut not_configured = engine(vec![FakeTurn::Refusal(TurnRefusal::NotConfigured {
            reason_code: "no_provider_route".to_owned(),
            detail: "nothing is configured".to_owned(),
        })]);
        let result = take(&mut not_configured, &scope("session-fixture", "project"));
        assert!(matches!(
            result.outcome,
            EngineOutcome::Failed { ref reason_code, .. } if reason_code == "no_provider_route"
        ));
        assert_eq!(result.dispatch.disposition, DispatchDisposition::Local);

        let mut unavailable = engine(vec![FakeTurn::Refusal(TurnRefusal::Unavailable {
            reason_code: "breaker_open".to_owned(),
            detail: "route is cooling down".to_owned(),
        })]);
        let result = take(&mut unavailable, &scope("session-fixture", "project"));
        assert!(matches!(
            result.outcome,
            EngineOutcome::NeedsAttention { ref reason_code, .. } if reason_code == "breaker_open"
        ));
        assert_eq!(result.dispatch.disposition, DispatchDisposition::Local);
        assert_eq!(
            TurnRefusal::Unavailable {
                reason_code: String::new(),
                detail: String::new()
            }
            .label(),
            "unavailable"
        );
    }

    #[test]
    fn a_binding_refuses_identifiers_that_are_not_opaque() {
        assert!(OrchestratorBinding::new("session-1", "project").is_some());
        assert!(OrchestratorBinding::new("", "project").is_none());
        assert!(OrchestratorBinding::new("session-1", "../escape").is_none());
    }

    #[test]
    fn an_indeterminate_receipt_is_carried_through_for_the_host_to_act_on() {
        let mut adapter = engine(vec![FakeTurn::dispatched(
            EngineOutcome::Failed {
                reason_code: "stream_broken".to_owned(),
                detail: "connection dropped".to_owned(),
            },
            DispatchDisposition::Indeterminate,
            Some("attempt-9"),
            None,
        )]);
        let result = take(&mut adapter, &scope("session-fixture", "project"));
        assert_eq!(
            result.dispatch.disposition,
            DispatchDisposition::Indeterminate
        );
        assert!(!result.dispatch.disposition.may_advance());
    }
}
