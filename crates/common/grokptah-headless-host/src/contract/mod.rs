//! Crate-local copies of the host-neutral contract types this seam was written
//! against.
//!
//! Integration's `crates/codegen/grokptah-agent-sdk` is a different read-only
//! observatory and does not export these symbols. This module keeps the host
//! self-contained so that SDK crate stays unmodified.

/// Stable contract identifier advertised by this host.
pub const CONTRACT_VERSION: &str = "grokptah.capabilities.v1";

/// Versioned capability discovery types.
pub mod capability;
/// Stable cross-product error categories.
pub mod error;
/// Durable run, review, and event types.
pub mod run;

pub use capability::{CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier};
pub use error::{ErrorCode, ErrorEnvelope, ErrorEventRange};
pub use run::{
    Bounds, ChangedFile, DurableRun, DurableRunState, ExecutionMode, IdempotencyKey, ReviewReceipt,
    RunEvent, RunEventPage, RunNotification, RunScope, SubmitTaskRequest,
};
