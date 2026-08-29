//! Cancellation that proves the turn is actually idle.
//!
//! Flipping a `CancellationToken` says only that a request was made. It does
//! not say the tool subprocess exited, the subagent stopped writing, or the
//! provider round finished. `main` reports a turn cancelled on the strength of
//! the token alone, which is how a "cancelled" run keeps producing effects.
//!
//! Here a cancellation reaches [`CancelStatus::Cancelled`] only when the
//! turn's effect registry shows nothing active, and the evidence for that is a
//! [`TurnIdleProof`] which has no other constructor.
//!
//! Like [`super::effects`], this is bookkeeping over the host's own work. It
//! grants nothing and proves nothing about authority.

use std::fmt;

use super::effects::{EffectKind, EffectRegistry};

/// Evidence that the turn has no effect in flight.
///
/// Only [`CancellationLedger::prove_idle`] can mint one, and only when the
/// registry agrees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnIdleProof {
    /// Effects that were running when cancellation was requested and have since
    /// stopped. Their *outcome* is not asserted here — only that they ended.
    pub(crate) effects_stopped: usize,
    /// Effects that were registered but never started, so provably did nothing.
    pub(crate) effects_never_started: usize,
}

/// Where a cancellation has got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CancelStatus {
    /// Nobody asked.
    NotRequested,
    /// Asked, but effects are still active. The turn is *not* cancelled yet.
    Pending {
        active: usize,
        running: usize,
        externally_visible: usize,
    },
    /// Asked, and the turn is proven idle.
    Cancelled(TurnIdleProof),
}

impl CancelStatus {
    /// Whether it is honest to tell an operator the turn has stopped.
    pub(crate) fn is_settled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }
}

impl fmt::Display for CancelStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRequested => f.write_str("not requested"),
            Self::Pending {
                active, running, ..
            } => write!(f, "cancelling; {active} active, {running} running"),
            Self::Cancelled(_) => f.write_str("cancelled"),
        }
    }
}

/// Tracks one turn's cancellation.
#[derive(Debug, Default)]
pub(crate) struct CancellationLedger {
    requested: bool,
    running_at_request: usize,
    never_started_at_request: usize,
}

impl CancellationLedger {
    /// Ask the turn to stop.
    ///
    /// Idempotent: a second request must not restate the effect counts that
    /// were true when the first one arrived.
    pub(crate) fn request(&mut self, registry: &mut EffectRegistry) {
        if self.requested {
            return;
        }
        self.requested = true;
        self.running_at_request = registry.running_count();
        self.never_started_at_request = registry.registered_count();
        // Refuse new effects immediately; the ones already running are drained.
        registry.begin_quiescing();
    }

    pub(crate) fn requested(&self) -> bool {
        self.requested
    }

    /// The honest current status.
    pub(crate) fn status(&self, registry: &EffectRegistry) -> CancelStatus {
        if !self.requested {
            return CancelStatus::NotRequested;
        }
        let active = registry.active_count();
        if active > 0 {
            return CancelStatus::Pending {
                active,
                running: registry.running_count(),
                externally_visible: registry
                    .records()
                    .filter(|e| e.state.is_active() && e.kind.externally_visible())
                    .count(),
            };
        }
        CancelStatus::Cancelled(TurnIdleProof {
            effects_stopped: self.running_at_request,
            effects_never_started: self.never_started_at_request,
        })
    }

    /// Kinds of effect still holding the turn open.
    pub(crate) fn blocking_kinds(&self, registry: &EffectRegistry) -> Vec<EffectKind> {
        registry.active_kinds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable::effects::EffectKind;

    #[test]
    fn a_cancel_during_an_active_effect_is_not_settled() {
        let mut registry = EffectRegistry::default();
        let effect = registry
            .register(EffectKind::ToolCall, "run_terminal_cmd")
            .expect("registered");
        registry.start(&effect).expect("started");

        let mut cancel = CancellationLedger::default();
        cancel.request(&mut registry);

        let status = cancel.status(&registry);
        assert!(!status.is_settled(), "a live effect is not an idle turn");
        assert_eq!(
            status,
            CancelStatus::Pending {
                active: 1,
                running: 1,
                externally_visible: 1
            }
        );
        assert!(!cancel.status(&registry).is_settled());
        assert_eq!(cancel.blocking_kinds(&registry), vec![EffectKind::ToolCall]);

        registry.cancel(&effect).expect("effect stopped");
        let CancelStatus::Cancelled(proof) = cancel.status(&registry) else {
            panic!("with nothing in flight the turn is provably idle");
        };
        assert_eq!(proof.effects_stopped, 1);
    }

    #[test]
    fn a_registered_effect_that_never_started_still_blocks_settlement() {
        let mut registry = EffectRegistry::default();
        let pending = registry
            .register(EffectKind::ToolCall, "queued")
            .expect("registered");
        let mut cancel = CancellationLedger::default();
        cancel.request(&mut registry);
        assert!(!cancel.status(&registry).is_settled());
        registry.cancel(&pending).expect("cancelled before start");
        let CancelStatus::Cancelled(proof) = cancel.status(&registry) else {
            panic!("idle");
        };
        assert_eq!(proof.effects_never_started, 1);
        assert_eq!(proof.effects_stopped, 0);
    }

    #[test]
    fn cancellation_refuses_new_effects_and_is_idempotent() {
        let mut registry = EffectRegistry::default();
        let mut cancel = CancellationLedger::default();
        cancel.request(&mut registry);
        assert!(registry.register(EffectKind::ToolCall, "late").is_err());
        cancel.request(&mut registry);
        let CancelStatus::Cancelled(proof) = cancel.status(&registry) else {
            panic!("idle");
        };
        assert_eq!(proof.effects_stopped, 0);
    }

    #[test]
    fn a_turn_nobody_cancelled_is_not_reported_as_cancelled() {
        let registry = EffectRegistry::default();
        let cancel = CancellationLedger::default();
        assert_eq!(cancel.status(&registry), CancelStatus::NotRequested);
        assert!(!cancel.requested());
    }
}
