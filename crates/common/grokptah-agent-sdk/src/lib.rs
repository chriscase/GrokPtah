//! Host-neutral contracts for consuming GrokPtah from another product.
//!
//! This crate contains versioned serializable DTOs only. It deliberately has
//! no Tauri, provider, filesystem, network, credential, or execution policy
//! dependency. A desktop adapter or trusted web broker owns those concerns.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

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
    ExternalWorkerStreamState, EXTERNAL_WORKER_CONTRACT_VERSION,
    EXTERNAL_WORKER_STREAMING_SUPPORTED,
};
pub use run::{
    Bounds, ChangedFile, DurableRun, DurableRunState, ExecutionMode, IdempotencyKey, ReviewReceipt,
    RunEvent, RunEventPage, RunNotification, RunScope, SubmitTaskRequest,
};
