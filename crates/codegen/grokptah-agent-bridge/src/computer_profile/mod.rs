//! Adaptive Computer Use execution profiles (#435).
//!
//! One Computer Use capability, three profiles — **Economy**, **Balanced**, and
//! **High Assurance** — and exactly one safety contract shared by all of them.
//!
//! The layering matches the rest of Computer Use: this module sits *below* the
//! cockpit and *above* the model transport, and it is provider-neutral. It
//! decides how much observation and how many model calls a run may spend, and
//! when a run must escalate or stop. It decides nothing about authority: the
//! grant, the lease, target identity, observation freshness, sensitivity, and
//! host-side revalidation all remain with `computer_use::policy`, which runs
//! again immediately before any dispatch regardless of profile.
//!
//! ```text
//!   cockpit / headless caller
//!            |
//!            v
//!   AdaptiveController  --(TurnPermit: profile + budget)-->  computer_agent
//!            |                                                    |
//!   AdaptivePolicyEngine  <-- RuntimeSignal --                    v
//!            |                                          one proposal validator
//!            v                                                    |
//!   AdaptiveProfileProjection (operator truth)                    v
//!                                                     computer_use::policy (kernel)
//! ```
//!
//! # The three things this module refuses to do
//!
//! - **Widen anything.** Budgets are proven monotonic and kernel-bounded at
//!   compile time in [`profile`]. Escalation buys more observation and more
//!   attempts, never more authority.
//! - **Vary safety by profile.** [`SafetyFloor`] has one value,
//!   [`SafetyFloor::REQUIRED`], and no per-profile constructor. This is the
//!   structural fix for the inversion found in the #453 donor candidate, where
//!   the richest profile disabled the host verification the cheapest one
//!   required.
//! - **Guess.** Every runtime signal resolves to escalate or stop. A task that
//!   needs more assurance than the evidence supports stops and says so, rather
//!   than relabelling an unqualified model.
//!
//! # Naming
//!
//! `economy`, `balanced`, and `high_assurance` are the canonical identifiers,
//! per issue #435 and the #446 decision packet. `efficient` and `frontier` are
//! accepted on ingest so historical session metadata keeps deserializing, and
//! are canonicalized immediately. They are never emitted and never enumerated
//! as additional modes.

pub mod capability;
pub mod controller;
pub mod policy;
pub mod profile;
pub mod projection;
pub mod record;
pub mod risk;

pub use capability::{
    CapabilityAttribution, CapabilityEvidence, CapabilityGeneration, HostCapabilityEvidence,
    ModelCapabilityEvidence, OperatorCapabilityPolicy, HOST_INDEPENDENT_VERIFIER_AVAILABLE,
};
pub use controller::{AdaptiveController, ControllerError, ObservationFingerprint, TurnPermit};
pub use policy::{
    AdaptivePolicyEngine, PolicyOutcome, PolicyStop, ProfileDecision, ProfileReason,
    ProfileTransition, RuntimeSignal,
};
pub use profile::{
    AdaptiveProfile, ObservationDetail, ProfileBudget, ProfileTokenKind, SafetyFloor,
    CANONICAL_PROFILE_NAMES,
};
pub use projection::{project_adaptive, AdaptiveProfileProjection};
pub use record::{
    AdaptiveLifecycle, AdaptiveRecord, CostLedger, EscalationRecord, TerminalOutcome,
};
pub use risk::{classify_objective, classify_task, TaskRisk};
