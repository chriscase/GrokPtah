//! Provider-neutral durable swarm control plane.
//!
//! Today a GrokPtah coordinator spawns parallel children by firing background
//! subagent tasks: the fan-out is real, but it lives in one process's memory.
//! There is no dependency graph, no durable record of what was dispatched, no
//! admission bound, and no answer to "did that child start?" after a restart.
//! This crate is the smallest coherent abstraction that fixes those four gaps
//! without touching the existing task tool.
//!
//! # What it is
//!
//! A [`SwarmSpec`] declares an objective, the workers that may run, and a task
//! graph. [`SwarmController`] turns that into durable [`SwarmState`] and
//! advances it. Everything is a plain serializable value: the crate owns no
//! threads, opens no sockets, reads no clock, and generates no randomness.
//! Callers pass `now` in and persist the state out, which is what makes the
//! whole state machine replayable and testable.
//!
//! # What it is not
//!
//! It is not a second execution queue, and it grants nothing. Every authority
//! in these types is a *requirement* or a *reference* to an authority issued
//! elsewhere:
//!
//! * Worker capabilities are a closed set with no browser and no raw-host
//!   variant, so no specification can express either.
//! * A mutating worker must require worktree isolation, matching the
//!   repository's existing subagent isolation rule.
//! * Computer Use is reachable only through an operator-issued lease
//!   reference, which this crate validates and records but never mints.
//! * A provider, model, role, or capability is usable only if the measured
//!   [`ProviderCatalog`] names it. An empty catalog admits nothing.
//!
//! # Restart safety
//!
//! Dispatch is two-phase. [`SwarmController::plan_dispatches`] proposes;
//! [`SwarmController::record_dispatch_requested`] writes the durable record,
//! [`SwarmController::claim_dispatch_spawn`] gives one caller the spawn right,
//! and only that winner may spawn a child. Dispatch identities are derived from
//! `(swarm, task, attempt)`, so replaying a planning pass proposes the
//! identifier already on disk instead of minting a second one.
//!
//! A crash between the write and the spawn leaves a `Requested` record with no
//! acknowledgement. [`SwarmController::recover`] marks exactly those uncertain,
//! and an uncertain dispatch is never resent on a guess — only
//! [`SwarmController::reconcile_uncertain`] carrying positive evidence can
//! resolve it. Absence of evidence keeps the task parked and keeps its
//! capacity reserved.
//!
//! # Example
//!
//! ```no_run
//! # use chrono::Utc;
//! # use grokptah_swarm_control_plane::{SwarmController, SwarmSpec};
//! # fn run(spec: SwarmSpec) -> Result<(), Box<dyn std::error::Error>> {
//! let now = Utc::now();
//! let mut swarm = SwarmController::new(spec, now)?;
//!
//! for intent in swarm.plan_dispatches(now) {
//!     // Durable write happens first; the child is spawned only afterwards.
//!     let record = swarm.record_dispatch_requested(&intent, None, now)?;
//!     let claim = swarm.claim_dispatch_spawn(&record.dispatch_id, now)?;
//!     if claim.won {
//!         // Spawn, then acknowledge the claimed dispatch.
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod error;
mod ids;
mod policy;
mod projection;
mod scheduler;
mod spec;
mod state;
mod store;
mod validate;

pub use error::{SwarmError, SwarmErrorCode, SwarmResult};
pub use ids::{
    CredentialRef, DispatchId, ExternalRefId, LeaseId, MAX_ID_BYTES, ModelId, ProviderId, SwarmId,
    TaskId, WorkerId,
};
pub use policy::{
    AdmissionPolicy, BudgetPolicy, FailurePolicy, MAX_DEPENDENCIES, MAX_FAN_OUT, MAX_IN_FLIGHT,
    MAX_REVIEWERS, MAX_TASKS, MAX_TOTAL_DISPATCHES, MAX_WALL_CLOCK_SECS, MAX_WORKERS, QuorumRule,
    ReviewGate,
};
pub use projection::{
    EvidenceProjection, EvidenceRow, MAX_PROJECTED_EVIDENCE_BYTES, MAX_PROJECTED_LINE_BYTES,
    MAX_PROJECTED_OBJECTIVE_BYTES, SwarmProgressProjection, TaskProgressRow, TaskStateCounts,
    project_evidence, project_progress,
};
pub use scheduler::{RecoveryReport, SpawnClaim, SwarmController};
pub use spec::{
    ComputerUseActionClass, ComputerUseLeaseRef, ComputerUseRequirement, IsolationRequirement,
    LeaseIssuer, MAX_CATALOG_ENTRIES, MAX_INSTRUCTIONS_BYTES, MAX_OBJECTIVE_BYTES, MAX_TITLE_BYTES,
    ProviderCatalog, ProviderCatalogEntry, SWARM_SCHEMA_VERSION, SwarmSpec, TaskKind, TaskSpec,
    WorkerCapability, WorkerRole, WorkerSpec,
};
pub use state::{
    DispatchIntent, DispatchProbe, DispatchRecord, DispatchState, EvidenceEntry,
    MAX_EVIDENCE_DETAIL_BYTES, MAX_EVIDENCE_ENTRIES, MAX_EVIDENCE_LABEL_BYTES, MAX_REASON_BYTES,
    MAX_SUMMARY_BYTES, ReviewVerdict, SwarmLifecycle, SwarmState, TaskOutcome, TaskRecord,
    TaskResult, TaskState, derive_dispatch_id,
};
pub use store::{DurableSwarmStore, InMemorySwarmStore, LeaseClaim};
pub use validate::validate_swarm_spec;

// Re-exported so a consumer can build a [`WorkerSpec`] without taking a direct
// dependency on the tool-types crate. These are the existing subagent
// capability and isolation vocabularies, reused rather than redefined.
pub use xai_tool_types::{SubagentCapabilityMode, SubagentIsolationMode};
