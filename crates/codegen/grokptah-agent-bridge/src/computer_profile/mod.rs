//! Adaptive Computer Use execution profiles (#435, #272, #472).
//!
//! This is one policy/evidence layer above the existing `computer_use` kernel.
//! It never dispatches actions, creates authority, or replaces the durable
//! Computer Run store. Profile changes are bounded, explicit, audited through
//! the run's adaptive state, and fail closed when canonical authority evidence
//! is absent.

mod adapters;
pub mod authority;
pub mod capability;
pub mod controller;
pub mod policy;
pub mod profile;
pub mod projection;
pub mod replay;
pub mod risk;

pub(crate) use adapters::{
    AdaptiveObservationAdapter, ProviderImageInput, SemanticHeadlessAdapter, VisualGroundingAdapter,
};
pub use authority::{
    AdaptiveAuthoritySnapshot, AuthorityFailure, CanonicalAuthority, ProviderAttemptReceipt,
    ProviderAttemptRequest,
};
pub use capability::{
    CapabilityAttribution, CapabilityEvidence, HostCapabilityEvidence, ModelCapabilityEvidence,
};
pub use controller::{
    AdaptiveController, AdaptiveEvidenceEvent, AdaptiveEvidenceKind, AdaptiveRunState,
    AdaptiveSpend, ControllerError, EscalationRecord, ObservationFingerprint, TerminalKind,
    TerminalOutcome, TurnPermit,
};
pub use policy::{
    AdaptivePolicyEngine, PolicyOutcome, PolicyStop, ProfileDecision, ProfileReason,
    ProfileTransition, RuntimeSignal, TaskPolicy,
};
pub use profile::{
    AdaptiveProfile, ObservationDetail, ProfileBudget, ProfileTokenKind, SafetyFloor,
    CANONICAL_PROFILE_NAMES,
};
pub use projection::{
    project_adaptive, AdaptiveProfileProjection, BudgetProjection, CapabilityEvidenceProjection,
    CostProjection, EscalationProjection, TerminalProjection,
};
pub use replay::{ReplayError, ReplayEvent, ReplayEventKind, ReplaySummary, ReplayVerifier};
pub use risk::{classify_objective, classify_task, TaskRisk};
