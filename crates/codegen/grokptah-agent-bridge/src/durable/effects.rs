//! Registered-before-start supervision of a turn's in-flight effects.
//!
//! This is bookkeeping over the host's own work, not an authority. It grants
//! nothing, seals nothing, and cannot be presented as proof of anything to a
//! caller outside this crate — principal identity, capability generations and
//! the physical-send lattice all belong to the canonical G1–G4 spine (#497).
//! What it does is answer one question the host currently cannot: *is anything
//! from this turn still running?*
//!
//! Every effect that can outlive the call that started it is registered
//! **before** it starts. The ordering is what makes the answer trustworthy: an
//! effect that could start without a registration leaves nothing behind, so a
//! crash mid-effect is indistinguishable from an effect that never ran.
//!
//! The rule is enforced by the type system. [`EffectRegistry::register`] is the
//! only source of an [`EffectTicket`], and [`EffectRegistry::start`] requires
//! one, so an unregistered start does not compile.

use std::collections::BTreeMap;
use std::fmt;

/// Ceiling on effects supervised at once for one turn. Bounds growth.
pub(crate) const MAX_SUPERVISED_EFFECTS: usize = 256;

/// What kind of effect is being supervised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EffectKind {
    ToolCall,
    ProviderSend,
}

impl EffectKind {
    /// Whether an interrupted effect of this kind may have changed something
    /// outside the host.
    pub(crate) fn externally_visible(self) -> bool {
        matches!(self, Self::ToolCall | Self::ProviderSend)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::ProviderSend => "provider_send",
        }
    }
}

/// Lifecycle of one supervised effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectState {
    /// Registered, not yet started. Nothing has run.
    Registered,
    /// Running. An interruption here cannot prove whether it landed.
    Running,
    /// Finished normally.
    Finished,
    /// Stopped by cancellation.
    Cancelled,
}

impl EffectState {
    pub(crate) fn is_active(self) -> bool {
        matches!(self, Self::Registered | Self::Running)
    }
}

/// One supervised effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EffectRecord {
    pub(crate) id: u64,
    pub(crate) kind: EffectKind,
    pub(crate) state: EffectState,
    /// Opaque label for operator projections. A tool name, never a path,
    /// prompt, credential or argument value.
    pub(crate) label: String,
}

/// Proof that an effect was registered. Required to start it.
///
/// Deliberately not `Clone`: one registration is one effect.
#[derive(Debug)]
pub(crate) struct EffectTicket {
    id: u64,
}

/// Refusals from the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectError {
    AtCapacity,
    Unknown,
    IllegalTransition {
        from: EffectState,
        to: EffectState,
    },
    /// New effects are refused because the turn is stopping.
    Quiescing,
}

impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AtCapacity => f.write_str("effect supervision is at capacity"),
            Self::Unknown => f.write_str("unknown effect"),
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal effect transition {from:?} -> {to:?}")
            }
            Self::Quiescing => f.write_str("turn is stopping; new effects are refused"),
        }
    }
}

/// Supervises one turn's effects.
#[derive(Debug, Default)]
pub(crate) struct EffectRegistry {
    effects: BTreeMap<u64, EffectRecord>,
    next_id: u64,
    quiescing: bool,
}

impl EffectRegistry {
    /// Register an effect before it starts.
    pub(crate) fn register(
        &mut self,
        kind: EffectKind,
        label: impl Into<String>,
    ) -> Result<EffectTicket, EffectError> {
        if self.quiescing {
            return Err(EffectError::Quiescing);
        }
        if self.active_count() >= MAX_SUPERVISED_EFFECTS {
            return Err(EffectError::AtCapacity);
        }
        let id = self.next_id.saturating_add(1);
        self.next_id = id;
        self.effects.insert(
            id,
            EffectRecord {
                id,
                kind,
                state: EffectState::Registered,
                label: label.into(),
            },
        );
        Ok(EffectTicket { id })
    }

    /// Start a registered effect. The ticket is the proof of registration.
    pub(crate) fn start(&mut self, ticket: &EffectTicket) -> Result<(), EffectError> {
        self.transition(ticket.id, EffectState::Running)
    }

    pub(crate) fn finish(&mut self, ticket: &EffectTicket) -> Result<(), EffectError> {
        self.transition(ticket.id, EffectState::Finished)
    }

    pub(crate) fn cancel(&mut self, ticket: &EffectTicket) -> Result<(), EffectError> {
        self.transition(ticket.id, EffectState::Cancelled)
    }

    /// Refuse new registrations. Running effects continue and are drained,
    /// never abandoned — abandoning them is what leaves an effect running
    /// behind a turn reported as cancelled.
    pub(crate) fn begin_quiescing(&mut self) {
        self.quiescing = true;
    }

    pub(crate) fn active_count(&self) -> usize {
        self.effects
            .values()
            .filter(|e| e.state.is_active())
            .count()
    }

    pub(crate) fn running_count(&self) -> usize {
        self.effects
            .values()
            .filter(|e| e.state == EffectState::Running)
            .count()
    }

    pub(crate) fn registered_count(&self) -> usize {
        self.effects
            .values()
            .filter(|e| e.state == EffectState::Registered)
            .count()
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = &EffectRecord> {
        self.effects.values()
    }

    /// Kinds still active, for an operator projection.
    pub(crate) fn active_kinds(&self) -> Vec<EffectKind> {
        let mut kinds: Vec<EffectKind> = self
            .effects
            .values()
            .filter(|e| e.state.is_active())
            .map(|e| e.kind)
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }

    fn transition(&mut self, id: u64, to: EffectState) -> Result<(), EffectError> {
        let effect = self.effects.get_mut(&id).ok_or(EffectError::Unknown)?;
        let legal = matches!(
            (effect.state, to),
            (EffectState::Registered, EffectState::Running)
                | (EffectState::Registered, EffectState::Cancelled)
                | (EffectState::Running, EffectState::Finished)
                | (EffectState::Running, EffectState::Cancelled)
        );
        if !legal {
            return Err(EffectError::IllegalTransition {
                from: effect.state,
                to,
            });
        }
        effect.state = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_effect_must_be_registered_before_it_can_start() {
        let mut registry = EffectRegistry::default();
        let ticket = registry
            .register(EffectKind::ToolCall, "run_terminal_cmd")
            .expect("registered");
        assert_eq!(registry.registered_count(), 1);
        assert_eq!(registry.running_count(), 0);
        registry.start(&ticket).expect("started");
        assert_eq!(registry.running_count(), 1);
        registry.finish(&ticket).expect("finished");
        assert_eq!(registry.active_count(), 0);
        assert!(matches!(
            registry.start(&ticket),
            Err(EffectError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn supervision_is_bounded_and_quiescing_refuses_new_work() {
        let mut registry = EffectRegistry::default();
        let mut tickets = Vec::new();
        for index in 0..MAX_SUPERVISED_EFFECTS {
            tickets.push(
                registry
                    .register(EffectKind::ToolCall, format!("tool-{index}"))
                    .expect("within capacity"),
            );
        }
        assert_eq!(
            registry
                .register(EffectKind::ToolCall, "one-too-many")
                .unwrap_err(),
            EffectError::AtCapacity
        );
        registry.start(&tickets[0]).unwrap();
        registry.finish(&tickets[0]).unwrap();
        registry
            .register(EffectKind::ToolCall, "fits now")
            .expect("freed");

        registry.begin_quiescing();
        assert_eq!(
            registry.register(EffectKind::ToolCall, "late").unwrap_err(),
            EffectError::Quiescing
        );
    }

    #[test]
    fn every_supervised_kind_is_externally_visible_and_named() {
        // Both kinds can change something outside the host, so an interrupted
        // one is never assumed harmless.
        for kind in [EffectKind::ToolCall, EffectKind::ProviderSend] {
            assert!(kind.externally_visible());
            assert!(!kind.as_str().is_empty());
        }
        assert_eq!(EffectKind::ToolCall.as_str(), "tool_call");
        assert_eq!(EffectKind::ProviderSend.as_str(), "provider_send");
    }
}
