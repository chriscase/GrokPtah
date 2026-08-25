//! Host-neutral contracts for consuming GrokPtah from another product.
//!
//! This crate contains versioned serializable DTOs only. It deliberately has
//! no Tauri, provider, filesystem, network, credential, or execution policy
//! dependency. A desktop adapter or trusted web broker owns those concerns.
//! Implementation modules are private; consumers must use the crate-root
//! re-exports and must not reach through paths such as `external_worker`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod capability;
mod computer;
mod error;
mod external_worker;
mod redact;
mod run;

/// Stable contract identifier advertised during MCP initialization.
pub const CONTRACT_VERSION: &str = "grokptah.capabilities.v1";

pub use capability::{CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier};
pub use computer::{
    ComputerActionClass, ComputerControlRequest, ComputerControlResponse, ComputerEvent,
    ComputerEventDetail, ComputerEventPage, ComputerRunScope,
};
pub use error::{ErrorCode, ErrorEnvelope, ErrorEventRange};
pub use external_worker::{
    EXTERNAL_WORKER_CONTRACT_VERSION, ExternalWorkerArtifact, ExternalWorkerEvent,
    ExternalWorkerExecutionMode, ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest,
    ExternalWorkerLaunchResult, ExternalWorkerListPage, ExternalWorkerListQuery,
    ExternalWorkerProvider, ExternalWorkerRecord, ExternalWorkerRunRecord, ExternalWorkerState,
    ExternalWorkerSummary, MAX_EXTERNAL_WORKER_LIST_LIMIT,
};
pub use run::{
    Bounds, ChangedFile, DurableRun, DurableRunState, ExecutionMode, IdempotencyKey, ReviewReceipt,
    RunEvent, RunEventPage, RunEventUpdate, RunNotification, RunScope, SubmitTaskRequest,
};
