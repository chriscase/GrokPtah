//! Host-neutral contracts for consuming GrokPtah from another product.
//!
//! This crate contains versioned serializable DTOs only. It deliberately has
//! no Tauri, provider, filesystem, network, credential, or execution policy
//! dependency. A desktop adapter or trusted web broker owns those concerns.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Redacted public authority projection. Not a grant constructor.
pub mod authority;
/// Versioned capability discovery types.
pub mod capability;
/// Lease- and revision-fenced Computer Use types.
pub mod computer;
/// Stable cross-product error categories.
pub mod error;
/// Provider-neutral contracts for external cloud or host-owned workers.
pub mod external_worker;
/// Durable run, review, and event types.
pub mod run;

/// Stable contract identifier advertised during MCP initialization.
pub const CONTRACT_VERSION: &str = "grokptah.capabilities.v1";

pub use authority::{
    PublicArtifactIdentity, PublicAuthorityProjection, PublicExecutionLifecycle, PublicGrantClass,
    PublicIdentity, PublicIdentityClass, PublicRevisionSet, PublicSendState,
    PUBLIC_AUTHORITY_CONTRACT_VERSION, PUBLIC_AUTHORITY_SCHEMA_VERSION,
};
pub use capability::{CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier};
pub use computer::{
    ComputerActionClass, ComputerControlRequest, ComputerControlResponse, ComputerEvent,
    ComputerEventPage, ComputerRunScope,
};
pub use error::{ErrorCode, ErrorEnvelope, ErrorEventRange};
pub use external_worker::{
    ExternalWorkerArtifact, ExternalWorkerEvent, ExternalWorkerExecutionMode,
    ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult,
    ExternalWorkerProvider, ExternalWorkerRecord, ExternalWorkerRunRecord, ExternalWorkerState,
    EXTERNAL_WORKER_CONTRACT_VERSION,
};
pub use run::{
    Bounds, ChangedFile, DurableRun, DurableRunState, ExecutionMode, IdempotencyKey, ReviewReceipt,
    RunEvent, RunEventPage, RunNotification, RunScope, SubmitTaskRequest,
};
