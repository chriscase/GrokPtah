//! The durable adaptive record: one per Computer Run, stored with the run.
//!
//! The first cut of this layer kept adaptive state in a process-local
//! `HashMap<Uuid, _>` keyed by **session**. That was wrong twice over. A
//! session outlives a Computer Run, so a second run inherited the first run's
//! profile, spend, and escalation history; and nothing survived a restart, so
//! the operator-facing account of what a run did evaporated exactly when it
//! mattered most — after a crash.
//!
//! [`AdaptiveRecord`] fixes both by being a field on [`ComputerRun`]. It is
//! written through the same crash-atomic store as the rest of the run, keyed
//! by run id rather than session, recovered by the same restart path, and
//! projected through the same read seam the cockpit and MCP already share.
//! There is no second ledger.
//!
//! # Every mutation is a pure transition
//!
//! Nothing here reaches for a clock, a lock, or the network. Each method takes
//! what it needs and returns what changed, so the service layer can persist
//! the result atomically and a test can drive an entire run without a store.
//!
//! # Legacy records fail closed
//!
//! `ComputerRun::adaptive` is `#[serde(default)]`, so a run written before
//! this field existed deserializes to `None`. `None` is not "no constraints";
//! it is "no adaptive authority has been established", and the host refuses to
//! spend a model call against it until a decision is recorded.

use serde::{Deserialize, Serialize};

use super::capability::CapabilityGeneration;
use super::policy::{ProfileDecision, ProfileReason};
use super::profile::AdaptiveProfile;
use super::risk::TaskRisk;

/// Escalation records retained per run. Bounded so a pathological run cannot
/// grow the durable record without limit; the ladder has two rungs, so this is
/// generous rather than tight.
pub const MAX_ESCALATION_HISTORY: usize = 32;

/// Where a run's adaptive lifecycle currently stands.
///
/// `InFlight` is durable on purpose: it is what makes a crash mid-turn
/// distinguishable from a clean stop after restart. Recovery turns it into
/// [`AdaptiveLifecycle::Interrupted`], never into a replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveLifecycle {
    /// A decision exists and no turn is in flight.
    Idle,
    /// A model call was admitted and has not yet been accounted for. A record
    /// found in this state after a restart was interrupted mid-turn.
    InFlight,
    /// The run ended honestly through policy.
    Stopped,
    /// The objective was satisfied and the host verified the postcondition.
    Completed,
    /// A restart cut a turn. Authority is cleared; nothing is replayed.
    Interrupted,
}

impl AdaptiveLifecycle {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Completed | Self::Interrupted)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::InFlight => "in_flight",
            Self::Stopped => "stopped",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// One escalation, attributable to exactly one signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRecord {
    pub from: AdaptiveProfile,
    pub to: AdaptiveProfile,
    pub reason: ProfileReason,
    /// Adaptive revision at which the escalation took effect, so the record
    /// orders against the run's own event stream.
    pub revision: u64,
}

/// What a run has spent.
///
/// Host-measured fields are always populated. Provider-reported fields are
/// [`Option`] and stay `None` until a provider actually reports usage; they
/// are never estimated, and there is no currency field, because this process
/// has no price table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostLedger {
    /// Provider attempts admitted. Incremented **before** the call, so a
    /// timeout, a transport failure, a malformed body, and a schema refusal
    /// all count exactly as much as a success: they all cost money and they
    /// all consumed budget.
    pub provider_attempts: u32,
    /// Attempts that produced a usable proposal.
    pub accepted_attempts: u32,
    /// Attempts that failed for any reason. `provider_attempts` is always the
    /// sum of this and `accepted_attempts`.
    pub failed_attempts: u32,
    /// Serialized observation bytes actually rendered for the model.
    pub observation_bytes: u64,
    /// Screenshot bytes sent to a model. Structurally always zero.
    pub screenshot_bytes: u64,
    /// Provider-reported prompt tokens, summed. Preserved even when the
    /// response that carried them failed to parse: the tokens were still
    /// billed.
    pub prompt_tokens: Option<u64>,
    /// Provider-reported completion tokens, summed. Same rule.
    pub completion_tokens: Option<u64>,
}

impl CostLedger {
    /// Records provider-reported usage. Called for failures too, because a
    /// response that arrived and then failed validation was still paid for;
    /// dropping its usage would make the cheapest profile look cheaper than it
    /// is exactly when it is misbehaving.
    pub fn add_usage(&mut self, prompt: Option<u64>, completion: Option<u64>) {
        if let Some(prompt) = prompt {
            self.prompt_tokens = Some(self.prompt_tokens.unwrap_or(0).saturating_add(prompt));
        }
        if let Some(completion) = completion {
            self.completion_tokens = Some(
                self.completion_tokens
                    .unwrap_or(0)
                    .saturating_add(completion),
            );
        }
    }
}

/// How a run ended, as the operator reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutcome {
    pub lifecycle: AdaptiveLifecycle,
    pub reason: ProfileReason,
    pub profile: AdaptiveProfile,
    /// Present when the run stopped because it needed a profile it could not
    /// have.
    pub required_profile: Option<AdaptiveProfile>,
}

/// The durable adaptive state of one Computer Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveRecord {
    /// Compare-and-swap witness. Advanced by every state change, so a caller
    /// holding a stale revision cannot apply anything.
    pub revision: u64,
    pub profile: AdaptiveProfile,
    /// The decision this run started from, kept verbatim so the projection can
    /// always explain the original selection even after escalations.
    pub decision: ProfileDecision,
    /// The capability generation the decision was made under. Revalidated on
    /// every turn; a mismatch stops the run.
    pub generation: CapabilityGeneration,
    /// The highest risk class this run has ever been asked to serve. A later,
    /// higher-risk objective raises it, and raising it re-runs selection —
    /// a run authorized for routine work never silently serves a destructive
    /// follow-up.
    pub risk_high_water: TaskRisk,
    pub lifecycle: AdaptiveLifecycle,
    pub escalations: Vec<EscalationRecord>,
    pub cost: CostLedger,
    /// Consecutive identical frames seen at the current profile.
    pub stationary_repeats: u32,
    /// Consecutive unusable model answers.
    pub uncertain_streak: u32,
    /// Postcondition failures. The second one halts rather than escalating.
    pub verification_failures: u32,
    /// Repairs spent inside the turn currently in flight.
    pub turn_repairs: u32,
    /// Whether the profile's element ceiling bounded the last rendered view.
    pub observation_truncated: bool,
    /// Structural digest of the most recent frame, hex. Opaque: it is derived
    /// from element identity and hashed label/value, never projected, and
    /// never sent to a model.
    pub last_frame_digest: Option<String>,
    pub terminal: Option<TerminalOutcome>,
}

impl AdaptiveRecord {
    /// Opens a record for a run whose profile the policy engine just selected.
    pub fn new(decision: ProfileDecision, generation: CapabilityGeneration) -> Self {
        Self {
            revision: 0,
            profile: decision.profile,
            risk_high_water: decision.risk,
            decision,
            generation,
            lifecycle: AdaptiveLifecycle::Idle,
            escalations: Vec::new(),
            cost: CostLedger::default(),
            stationary_repeats: 0,
            uncertain_streak: 0,
            verification_failures: 0,
            turn_repairs: 0,
            observation_truncated: false,
            last_frame_digest: None,
            terminal: None,
        }
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    /// Records an escalation, bounded.
    pub fn push_escalation(&mut self, record: EscalationRecord) {
        if self.escalations.len() >= MAX_ESCALATION_HISTORY {
            self.escalations.remove(0);
        }
        self.escalations.push(record);
    }

    /// A fresh profile gets a fresh look: the counters that describe "this
    /// profile keeps seeing the same thing" do not carry across a change of
    /// what the profile can see.
    pub fn reset_for_new_profile(&mut self) {
        self.stationary_repeats = 0;
        self.uncertain_streak = 0;
        self.turn_repairs = 0;
        self.observation_truncated = false;
        self.last_frame_digest = None;
    }

    /// Restart recovery. Fail-closed and lossy in exactly one direction: a
    /// turn that was in flight becomes interrupted and nothing is replayed.
    /// An already-terminal record keeps its original reason.
    pub fn recover_interrupted(&mut self) {
        self.bump();
        self.last_frame_digest = None;
        self.stationary_repeats = 0;
        self.turn_repairs = 0;
        if self.lifecycle.is_terminal() {
            return;
        }
        self.lifecycle = AdaptiveLifecycle::Interrupted;
        self.terminal = Some(TerminalOutcome {
            lifecycle: AdaptiveLifecycle::Interrupted,
            reason: ProfileReason::RunInterrupted,
            profile: self.profile,
            required_profile: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_profile::capability::{
        CapabilityAttribution, CapabilityEvidence, HostCapabilityEvidence, ModelCapabilityEvidence,
        OperatorCapabilityPolicy,
    };
    use crate::gateway_config::ComputerUseTier;

    fn generation() -> CapabilityGeneration {
        CapabilityGeneration::compute(
            "route-1",
            &crate::gateway_config::ModelCapabilities::default(),
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        )
    }

    fn decision() -> ProfileDecision {
        ProfileDecision {
            profile: AdaptiveProfile::Economy,
            reason: ProfileReason::RoutineTask,
            risk: TaskRisk::Routine,
            ceiling: AdaptiveProfile::Economy,
            evidence: CapabilityEvidence::new(
                ModelCapabilityEvidence {
                    tools: true,
                    image_input: false,
                    max_image_bytes: None,
                    tier: ComputerUseTier::SemanticAct,
                    attribution: CapabilityAttribution::Measured,
                    durable_authority: true,
                    session_measured: false,
                    synthetic_only: false,
                    generation: generation(),
                    declared_capability_trusted: false,
                },
                HostCapabilityEvidence::SEMANTIC_ONLY,
            ),
        }
    }

    #[test]
    fn a_record_round_trips_through_serde() {
        let record = AdaptiveRecord::new(decision(), generation());
        let wire = serde_json::to_string(&record).unwrap();
        let back: AdaptiveRecord = serde_json::from_str(&wire).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn failed_attempts_still_count_and_keep_their_usage() {
        let mut record = AdaptiveRecord::new(decision(), generation());
        record.cost.provider_attempts = 3;
        record.cost.failed_attempts = 2;
        record.cost.accepted_attempts = 1;
        // A response that arrived, reported usage, and then failed to parse
        // was still billed.
        record.cost.add_usage(Some(120), None);
        record.cost.add_usage(Some(30), Some(9));
        assert_eq!(record.cost.prompt_tokens, Some(150));
        assert_eq!(record.cost.completion_tokens, Some(9));
        assert_eq!(
            record.cost.provider_attempts,
            record.cost.failed_attempts + record.cost.accepted_attempts
        );
    }

    #[test]
    fn unreported_usage_stays_unknown_rather_than_zero() {
        let mut record = AdaptiveRecord::new(decision(), generation());
        record.cost.add_usage(None, None);
        assert_eq!(record.cost.prompt_tokens, None);
        assert_eq!(record.cost.completion_tokens, None);
    }

    #[test]
    fn recovery_interrupts_an_in_flight_turn_without_replaying() {
        let mut record = AdaptiveRecord::new(decision(), generation());
        record.lifecycle = AdaptiveLifecycle::InFlight;
        record.turn_repairs = 2;
        let before = record.revision;
        record.recover_interrupted();
        assert_eq!(record.lifecycle, AdaptiveLifecycle::Interrupted);
        assert!(record.revision > before, "revision must advance");
        assert_eq!(record.turn_repairs, 0);
        assert_eq!(record.cost.provider_attempts, 0, "nothing was replayed");
        assert_eq!(
            record.terminal.as_ref().map(|terminal| terminal.reason),
            Some(ProfileReason::RunInterrupted)
        );
    }

    #[test]
    fn recovery_preserves_an_existing_terminal_reason() {
        let mut record = AdaptiveRecord::new(decision(), generation());
        record.lifecycle = AdaptiveLifecycle::Stopped;
        record.terminal = Some(TerminalOutcome {
            lifecycle: AdaptiveLifecycle::Stopped,
            reason: ProfileReason::RepeatedStationarity,
            profile: AdaptiveProfile::Economy,
            required_profile: None,
        });
        record.recover_interrupted();
        assert_eq!(record.lifecycle, AdaptiveLifecycle::Stopped);
        assert_eq!(
            record.terminal.as_ref().map(|terminal| terminal.reason),
            Some(ProfileReason::RepeatedStationarity)
        );
    }

    #[test]
    fn escalation_history_is_bounded_in_the_durable_record() {
        let mut record = AdaptiveRecord::new(decision(), generation());
        for index in 0..(MAX_ESCALATION_HISTORY * 2) {
            record.push_escalation(EscalationRecord {
                from: AdaptiveProfile::Economy,
                to: AdaptiveProfile::Balanced,
                reason: ProfileReason::AmbiguousObservation,
                revision: index as u64,
            });
        }
        assert_eq!(record.escalations.len(), MAX_ESCALATION_HISTORY);
        // The bound drops the oldest, so the most recent history survives.
        assert_eq!(
            record.escalations.last().map(|entry| entry.revision),
            Some((MAX_ESCALATION_HISTORY * 2 - 1) as u64)
        );
    }
}
