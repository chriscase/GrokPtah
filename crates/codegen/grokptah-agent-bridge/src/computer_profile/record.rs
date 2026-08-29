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

use super::capability::CapabilityEvidence;
use super::capability::CapabilityGeneration;
use super::policy::{PolicyStop, ProfileDecision, ProfileReason};
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
    /// Attempts that failed for any reason. At rest, `provider_attempts` is the
    /// sum of this and `accepted_attempts`; while a turn is in flight — or
    /// after a restart cut one — exactly one attempt is counted and not yet
    /// resolved, because the attempt is counted before the request leaves.
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
    /// Whether the profile's element ceiling bounded the last rendered view.
    pub observation_truncated: bool,
    /// Structural digest of the most recent frame, hex. Opaque: it is derived
    /// from element identity and hashed label/value, never projected, and
    /// never sent to a model.
    pub last_frame_digest: Option<String>,
    pub terminal: Option<TerminalOutcome>,
    /// Opaque identity of the turn currently admitted, if any.
    ///
    /// Written only by [`AdaptiveController::begin_turn`], and cleared by every
    /// path that ends the run, escalates it, or recovers it after a restart. A
    /// sealed proposal binds this value, so authority admitted for one turn
    /// cannot be spent after the run has moved on — including a move that
    /// happened *while the provider was still thinking*.
    ///
    /// `#[serde(default)]` so a record written before this field existed loads
    /// as `None`, which is "no turn is admitted" — the fail-closed reading.
    #[serde(default)]
    pub active_permit: Option<String>,
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
            observation_truncated: false,
            last_frame_digest: None,
            terminal: None,
            active_permit: None,
        }
    }

    /// Opens a record for a run the policy engine refused before any profile
    /// was ever in force.
    ///
    /// A selection-time stop used to write no record at all, and that had two
    /// consequences. The operator's projection read `None` for a run the host
    /// had just refused, so the stop reason survived only as an audit line and
    /// never reached the cockpit, the MCP surface, or an SDK reader. And the
    /// run still read "no adaptive record", so the very next objective — at any
    /// lower risk — opened a fresh selection on the same authorized run, which
    /// is exactly the probe-then-proceed shape this layer exists to refuse.
    ///
    /// Both are closed by making the refusal durable, which is what every other
    /// terminal path here already does.
    pub fn stopped_at_selection(
        stop: &PolicyStop,
        evidence: CapabilityEvidence,
        risk: TaskRisk,
        generation: CapabilityGeneration,
    ) -> Self {
        let mut record = Self::new(
            ProfileDecision {
                profile: stop.profile,
                reason: stop.reason,
                risk,
                ceiling: stop.ceiling,
                evidence,
            },
            generation,
        );
        record.lifecycle = AdaptiveLifecycle::Stopped;
        record.terminal = Some(TerminalOutcome {
            lifecycle: AdaptiveLifecycle::Stopped,
            reason: stop.reason,
            profile: stop.profile,
            required_profile: stop.required_profile,
        });
        record
    }

    /// Check the record against its own invariants.
    ///
    /// A durable record is a file on disk. It can be truncated by a full
    /// filesystem, rewritten by a careless operator, restored from a backup
    /// taken mid-write, or edited on purpose. Every field here is an input to
    /// an authority decision, so a record that cannot be shown to be internally
    /// consistent must not be treated as a permissive one.
    ///
    /// Returns the first violated invariant, named, so a refusal says which.
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        // Attempts reconcile, with one exception that is not tampering: the
        // attempt is counted *before* the request leaves, so a turn that is
        // still in flight — or one a restart cut before it could be closed —
        // is legitimately counted and not yet resolved. At most one turn can be
        // in flight at a time, so at most one attempt may be outstanding.
        let resolved = self
            .cost
            .accepted_attempts
            .saturating_add(self.cost.failed_attempts);
        if resolved > self.cost.provider_attempts {
            return Err("more provider attempts were resolved than were made");
        }
        if self.cost.provider_attempts - resolved > 1 {
            return Err("more than one provider attempt is unaccounted for");
        }
        if self.cost.screenshot_bytes != 0 {
            return Err("a record reports screenshot bytes sent to a model");
        }
        // The profile may never exceed what the evidence can support. This is
        // the invariant an edited record would most want to break: it is the
        // one standing between a text-only route and High Assurance.
        if self.profile > self.decision.evidence.ceiling() {
            return Err("the recorded profile exceeds what its evidence can support");
        }
        if self.profile > self.decision.ceiling {
            return Err("the recorded profile exceeds the ceiling it was decided under");
        }
        if self.profile < self.decision.profile {
            return Err("the recorded profile is below the profile it was selected at");
        }
        if self.risk_high_water < self.decision.risk {
            return Err("the risk high-water mark is below the risk it was decided at");
        }
        if self.escalations.len() > MAX_ESCALATION_HISTORY {
            return Err("the escalation history exceeds its bound");
        }
        // The ladder is climbed one rung at a time, in order, ending where the
        // record says it is.
        let mut rung = self.decision.profile;
        for entry in &self.escalations {
            if entry.from != rung || entry.to <= entry.from {
                return Err("the escalation history is not a contiguous climb");
            }
            rung = entry.to;
        }
        if rung != self.profile {
            return Err("the escalation history does not end at the recorded profile");
        }
        if self.terminal.is_some() != self.lifecycle.is_terminal() {
            return Err("the terminal outcome and the lifecycle disagree");
        }
        if let Some(terminal) = &self.terminal {
            if terminal.lifecycle != self.lifecycle {
                return Err("the terminal outcome names a different lifecycle");
            }
            if self.active_permit.is_some() {
                return Err("a terminal record still holds an admitted turn");
            }
        }
        if self.lifecycle != AdaptiveLifecycle::InFlight && self.active_permit.is_some() {
            // A permit outlives the in-flight window on purpose — the answer it
            // admitted may still be applied once the turn is accounted for —
            // but only while the run is idle and able to accept it.
            if self.lifecycle != AdaptiveLifecycle::Idle {
                return Err("a record holds an admitted turn in a state that cannot spend one");
            }
        }
        Ok(())
    }

    /// Fail closed on a record that violates its own invariants.
    ///
    /// Called on every load, not only at restart, so a record corrupted or
    /// edited at any point stops being authority the next time it is read. The
    /// record is kept — the operator should be able to see that this happened
    /// and why — but it is converted to a terminal `record_invalid` stop, which
    /// every admission path already refuses.
    ///
    /// Returns the violated invariant when one was found.
    pub fn enforce_invariants(&mut self) -> Option<&'static str> {
        let violation = self.check_invariants().err()?;
        self.active_permit = None;
        self.lifecycle = AdaptiveLifecycle::Stopped;
        self.terminal = Some(TerminalOutcome {
            lifecycle: AdaptiveLifecycle::Stopped,
            reason: ProfileReason::RecordInvalid,
            profile: self.profile,
            required_profile: None,
        });
        Some(violation)
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
        // Whatever turn was admitted before the restart is not admitted now.
        self.active_permit = None;
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
        record.active_permit = Some("permit-before-the-crash".into());
        let before = record.revision;
        record.recover_interrupted();
        assert_eq!(record.lifecycle, AdaptiveLifecycle::Interrupted);
        assert!(record.revision > before, "revision must advance");
        assert_eq!(
            record.active_permit, None,
            "a turn admitted before the restart is not admitted after it"
        );
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
