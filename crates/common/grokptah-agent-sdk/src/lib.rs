//! Host-neutral contracts for consuming GrokPtah from another product.
//!
//! This crate contains versioned serializable DTOs only. It deliberately has
//! no Tauri, provider, filesystem, network, credential, or execution policy
//! dependency. A desktop adapter or trusted web broker owns those concerns.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Versioned, credential-free Grok Build account readiness facts.
pub mod account;
/// One provider attempt: what it was bound to, and whether it was sent.
pub mod attempt;
/// Versioned capability discovery types.
pub mod capability;
/// Lease- and revision-fenced Computer Use types.
pub mod computer;
/// Stable cross-product error categories.
pub mod error;
/// Provider-neutral contracts for external cloud or host-owned workers.
pub mod external_worker;
/// Fail-closed Grok Build launch truth.
pub mod launch;
/// Typed terminal outcomes for runs that could not succeed.
pub mod outcome;
/// Durable run, review, and event types.
pub mod run;

/// Stable contract identifier advertised during MCP initialization.
pub const CONTRACT_VERSION: &str = "grokptah.capabilities.v1";

pub use account::{
    AccountObservation, AccountReadiness, AccountReference, AccountReferenceSource,
    CredentialMethod, CredentialSource, ExpiryFacts, ExpiryStatus, GROK_ACCOUNT_CONTRACT_VERSION,
    GROK_ACCOUNT_SCHEMA_VERSION, GrokAccountFacts, MAX_ACCOUNT_REFERENCE_BYTES, ReadinessReason,
    RunAttribution,
};
pub use attempt::{
    AttemptIntent, AttemptRoute, AttemptSubject, AuthorityRevisions, BoundedId,
    GROK_ATTEMPT_CONTRACT_VERSION, GROK_ATTEMPT_SCHEMA_VERSION, MAX_ATTEMPT_IDENTIFIER_BYTES,
    ProviderAttempt, ProviderReceipts, Revision, SendOutcome, SendState, UsageReceipt,
};
pub use capability::{CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier};
pub use computer::{
    ComputerActionClass, ComputerControlRequest, ComputerControlResponse, ComputerEvent,
    ComputerEventPage, ComputerRunScope,
};
pub use error::{ErrorCode, ErrorEnvelope, ErrorEventRange};
pub use external_worker::{
    EXTERNAL_WORKER_CONTRACT_VERSION, ExternalWorkerArtifact, ExternalWorkerEvent,
    ExternalWorkerExecutionMode, ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest,
    ExternalWorkerLaunchResult, ExternalWorkerProvider, ExternalWorkerRecord,
    ExternalWorkerRunRecord, ExternalWorkerState,
};
pub use launch::{
    BaseCategory, CapabilityFacts, CapabilityProvenance, GROK_LAUNCH_CONTRACT_VERSION,
    GROK_LAUNCH_SCHEMA_VERSION, GrokLaunchTruth, LaunchObservation, LaunchReadiness, LaunchReason,
    LaunchRequirement, MAX_MODEL_REFERENCE_BYTES, ModelFacts, ModelReference, ModelStatus,
    ProviderClass, Refreshability, RequestDialect, RouteClass,
};
pub use outcome::{RunFailureKind, RunOutcomeClass, TerminalVerdict};
pub use run::{
    Bounds, ChangedFile, DurableRun, DurableRunState, ExecutionMode, IdempotencyKey, ReviewReceipt,
    RunEvent, RunEventPage, RunNotification, RunScope, SubmitTaskRequest,
};
