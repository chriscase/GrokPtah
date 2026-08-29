//! Registered-before-start effect supervision.
//!
//! Every effect that can outlive the call that started it — a provider send, a
//! tool call, a subagent, a background scan — is registered *before* it starts.
//! The ordering is what makes crash recovery possible at all: an effect that
//! could start without a registration leaves nothing behind to recover, so a
//! crash mid-effect is indistinguishable from an effect that never ran.
//!
//! The rule is enforced by the type system. [`EffectRegistry::register`] is the
//! only source of an [`EffectTicket`], and [`EffectRegistry::start`] requires
//! one, so an unregistered start does not compile.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Ceiling on effects supervised at once. Bounds resource growth.
pub const MAX_SUPERVISED_EFFECTS: usize = 256;

/// What kind of effect is being supervised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    ProviderSend,
    ToolCall,
    Subagent,
    BackgroundScan,
    ComputerUse,
}

impl EffectKind {
    /// Whether an interrupted effect of this kind may have changed something
    /// outside the host.
    pub fn externally_visible(self) -> bool {
        matches!(
            self,
            Self::ProviderSend | Self::ToolCall | Self::ComputerUse
        )
    }
}

/// Lifecycle of one supervised effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    /// Registered, not yet started. Nothing has run.
    Registered,
    /// Running. An interruption here cannot prove whether it landed.
    Running,
    /// Finished normally.
    Finished,
    /// Finished by cancellation.
    Cancelled,
}

impl EffectState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Registered | Self::Running)
    }
}

/// Durable shape of one supervised effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectRecord {
    pub id: u64,
    pub kind: EffectKind,
    pub state: EffectState,
    /// Opaque label for operator projections. Never a path, prompt or credential.
    pub label: String,
}

/// Proof that an effect was registered. Required to start it.
#[derive(Debug)]
pub struct EffectTicket {
    id: u64,
    kind: EffectKind,
}

impl EffectTicket {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn kind(&self) -> EffectKind {
        self.kind
    }
}

/// Refusals from the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectError {
    /// Too many effects are already supervised.
    AtCapacity,
    /// No effect with that id.
    Unknown,
    /// The effect is not in a state that permits this transition.
    IllegalTransition { from: EffectState, to: EffectState },
    /// New effects are refused because the turn is quiescing.
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
            Self::Quiescing => f.write_str("host is quiescing; new effects are refused"),
        }
    }
}

impl std::error::Error for EffectError {}

/// How an interrupted effect is classified after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    /// Registered but never started: provably nothing ran.
    pub never_started: Vec<u64>,
    /// Started and never finished: may have landed. Never auto-retried.
    pub indeterminate: Vec<u64>,
    /// Already terminal when the crash happened.
    pub settled: usize,
}

impl RecoveryReport {
    /// Whether recovery found anything whose outcome the host cannot determine.
    pub fn has_indeterminate(&self) -> bool {
        !self.indeterminate.is_empty()
    }
}

/// Supervises effects for one turn.
#[derive(Debug, Default)]
pub struct EffectRegistry {
    effects: BTreeMap<u64, EffectRecord>,
    next_id: u64,
    quiescing: bool,
}

impl EffectRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an effect before it starts.
    pub fn register(
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
        Ok(EffectTicket { id, kind })
    }

    /// Start a registered effect. The ticket is the proof of registration.
    pub fn start(&mut self, ticket: &EffectTicket) -> Result<(), EffectError> {
        self.transition(ticket.id, EffectState::Running)
    }

    pub fn finish(&mut self, ticket: &EffectTicket) -> Result<(), EffectError> {
        self.transition(ticket.id, EffectState::Finished)
    }

    pub fn cancel(&mut self, ticket: &EffectTicket) -> Result<(), EffectError> {
        self.transition(ticket.id, EffectState::Cancelled)
    }

    /// Refuse new registrations. Running effects continue.
    pub fn begin_quiescing(&mut self) {
        self.quiescing = true;
    }

    pub fn is_quiescing(&self) -> bool {
        self.quiescing
    }

    /// Effects that are registered or running.
    pub fn active_count(&self) -> usize {
        self.effects
            .values()
            .filter(|e| e.state.is_active())
            .count()
    }

    /// Effects that have actually started and not finished.
    pub fn running_count(&self) -> usize {
        self.effects
            .values()
            .filter(|e| e.state == EffectState::Running)
            .count()
    }

    pub fn record(&self, id: u64) -> Option<&EffectRecord> {
        self.effects.get(&id)
    }

    pub fn records(&self) -> impl Iterator<Item = &EffectRecord> {
        self.effects.values()
    }

    /// Classify what survived a crash.
    ///
    /// The distinction is the whole point of registering first: `Registered`
    /// proves the effect never ran, while `Running` is honestly indeterminate
    /// and is never auto-retried.
    pub fn recover(records: impl IntoIterator<Item = EffectRecord>) -> (Self, RecoveryReport) {
        let mut report = RecoveryReport {
            never_started: Vec::new(),
            indeterminate: Vec::new(),
            settled: 0,
        };
        let mut effects = BTreeMap::new();
        for record in records {
            match record.state {
                EffectState::Registered => report.never_started.push(record.id),
                EffectState::Running => report.indeterminate.push(record.id),
                EffectState::Finished | EffectState::Cancelled => report.settled += 1,
            }
            effects.insert(record.id, record);
        }
        let next_id = effects.keys().copied().max().unwrap_or(0);
        report.never_started.sort_unstable();
        report.indeterminate.sort_unstable();
        (
            Self {
                effects,
                next_id,
                quiescing: false,
            },
            report,
        )
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
