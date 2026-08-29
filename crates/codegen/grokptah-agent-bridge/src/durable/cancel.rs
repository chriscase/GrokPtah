//! Cancellation that proves the turn is actually idle.
//!
//! Flipping a cancellation token says only that a request was made. It does not
//! say that the provider send stopped, that the tool subprocess exited, or that
//! a subagent is no longer writing. Reporting `Cancelled` on the strength of the
//! token alone is how a "cancelled" run keeps producing effects.
//!
//! Here a cancellation reaches [`CancelStatus::Cancelled`] only when the effect
//! registry shows no active effect, and the proof of that is a
//! [`TurnIdleProof`] which cannot be constructed any other way.

use serde::{Deserialize, Serialize};
use std::fmt;

use super::effects::{EffectKind, EffectRegistry};

/// Why the turn is stopping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// A person asked for it.
    Operator,
    /// The host is shutting down.
    Shutdown,
    /// A bound was reached.
    BoundExhausted,
}

/// Evidence that the turn has no effect in flight.
///
/// Only [`CancellationLedger::prove_idle`] can mint one, and only when the
/// registry agrees.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnIdleProof {
    pub reason: CancelReason,
    /// Effects that were still running when cancellation was requested and have
    /// since stopped. Their outcome is not asserted here.
    pub effects_stopped: usize,
    /// Effects that never started, so provably did nothing.
    pub effects_never_started: usize,
}

/// Where a cancellation has got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CancelStatus {
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
    pub fn is_settled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }
}

impl fmt::Display for CancelStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRequested => f.write_str("not requested"),
            Self::Pending {
                active, running, ..
            } => {
                write!(f, "cancelling; {active} active, {running} running")
            }
            Self::Cancelled(_) => f.write_str("cancelled"),
        }
    }
}

/// Tracks one turn's cancellation.
#[derive(Debug, Default)]
pub struct CancellationLedger {
    requested: Option<CancelReason>,
    /// Effects observed running at the moment cancellation was requested.
    running_at_request: usize,
    never_started_at_request: usize,
}

impl CancellationLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the turn to stop. Idempotent: the first reason wins, so a shutdown
    /// racing an operator cancel does not rewrite the record.
    pub fn request(&mut self, reason: CancelReason, registry: &mut EffectRegistry) {
        if self.requested.is_some() {
            return;
        }
        self.requested = Some(reason);
        self.running_at_request = registry.running_count();
        self.never_started_at_request = registry
            .records()
            .filter(|e| e.state == super::effects::EffectState::Registered)
            .count();
        // Refuse new effects immediately; existing ones are drained, not
        // abandoned, because abandoning them is what leaves an effect running
        // behind a "cancelled" run.
        registry.begin_quiescing();
    }

    pub fn requested(&self) -> bool {
        self.requested.is_some()
    }

    /// The honest current status.
    pub fn status(&self, registry: &EffectRegistry) -> CancelStatus {
        let Some(reason) = self.requested else {
            return CancelStatus::NotRequested;
        };
        let active = registry.active_count();
        if active > 0 {
            let externally_visible = registry
                .records()
                .filter(|e| e.state.is_active() && e.kind.externally_visible())
                .count();
            return CancelStatus::Pending {
                active,
                running: registry.running_count(),
                externally_visible,
            };
        }
        CancelStatus::Cancelled(TurnIdleProof {
            reason,
            effects_stopped: self.running_at_request,
            effects_never_started: self.never_started_at_request,
        })
    }

    /// Mint the idle proof, or say what is still in flight.
    ///
    /// This is the only constructor of [`TurnIdleProof`].
    pub fn prove_idle(&self, registry: &EffectRegistry) -> Result<TurnIdleProof, CancelStatus> {
        match self.status(registry) {
            CancelStatus::Cancelled(proof) => Ok(proof),
            other => Err(other),
        }
    }

    /// Kinds of effect still active, for an operator projection.
    pub fn blocking_kinds(&self, registry: &EffectRegistry) -> Vec<EffectKind> {
        let mut kinds: Vec<EffectKind> = registry
            .records()
            .filter(|e| e.state.is_active())
            .map(|e| e.kind)
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }
}
