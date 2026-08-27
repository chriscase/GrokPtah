//! A runnable headless host for durable GrokPtah runs.
//!
//! This crate is the smallest production-shaped seam that can start, observe,
//! steer, pause, resume, and durably recover runs without the desktop app. It
//! owns process lifecycle, durable state, authority enforcement, and truthful
//! projection. It does not own model execution, credentials, or transport.
//!
//! # Authority tier
//!
//! Per [ADR-002], every surface must name its tier. This host is a **local host
//! authority for one home it owns exclusively**. Concretely:
//!
//! - *May:* own its own home, admit and bound its own runs, hold short-lived
//!   control leases, recover its own interrupted runs, and publish redacted
//!   projections.
//! - *May never:* write another GrokPtah home (the desktop home is refused by
//!   configuration), originate work by itself, hold or read credentials, grant
//!   Computer Use, resume interrupted work without an explicit operator action,
//!   or widen a capability it was not configured with.
//!
//! It satisfies the first service trigger in ADR-002 — work that must outlive
//! the operator's terminal — and deliberately not the second or third: it takes
//! an exclusive lock on a home of its own, and it accepts no off-box caller.
//!
//! # Fail-closed defaults
//!
//! Absence is never permission. An unconfigured capability is denied, a gated
//! capability without an explicit grant is denied, a request above a host
//! ceiling is refused rather than clamped, an unresolved escalation expires to
//! *deny*, an unknown request field is rejected, and a projection the public
//! contract would reject never leaves the host.
//!
//! # Determinism
//!
//! Time, identity, and execution all arrive through injected ports
//! ([`clock::Clock`], [`engine::RunEngine`]), so the whole lifecycle is
//! exercisable offline with no provider credential and no network.
//!
//! [ADR-002]: https://github.com/chriscase/GrokPtah/blob/main/docs/ADR-002-runtime-boundaries.md

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Needs-attention escalation and its default-deny resolution.
pub mod attention;
/// Capability and bounds admission.
pub mod authority;
/// Deterministic time.
pub mod clock;
/// Validated startup configuration.
pub mod config;
/// The operator control protocol.
pub mod control;
/// The run engine port and its deterministic offline implementation.
pub mod engine;
/// Host failures and their public envelopes.
pub mod error;
/// Host wiring: startup, admission, stepping, control, shutdown.
pub mod host;
/// Opaque identities and payload fingerprints.
pub mod identity;
/// Append-only bounded event journal.
pub mod journal;
/// Short-lived, revision-fenced control leases.
pub mod lease;
/// Process lifecycle and shutdown escalation.
pub mod lifecycle;
/// Exclusive ownership of the host home.
pub mod lock;
/// The adapter boundary between this host and an agent-loop orchestrator.
pub mod orchestration;
/// Truthful status, health, and receipt projections.
pub mod projection;
/// Write-boundary redaction.
pub mod redaction;
/// Durable records, idempotency, and restart recovery.
pub mod store;
/// Deterministic offline fixtures.
pub mod testing;

#[cfg(feature = "cli")]
/// Cross-platform OS signal wiring for the operator binary.
pub mod signal;

pub use config::{EngineSelection, HostConfig, HostLimits};
pub use control::{ControlCommand, ControlReply, ControlRequest, ControlResult};
pub use engine::{DispatchDisposition, DispatchReport, RunEngine, StepResult};
pub use error::{HostError, HostResult};
pub use host::{HeadlessHost, StartupReport, StopReport};
pub use identity::ExternalRef;
pub use lifecycle::{CancelSignal, HostState, ShutdownKind, ShutdownSignal};
pub use orchestration::{
    OrchestratedEngine, OrchestratorBinding, TurnOrchestrator, TurnReceipt, TurnRefusal,
    TurnRequest,
};
pub use projection::{HealthReport, HostRunStatus};
pub use store::{RunPhase, RunRecord};

/// Contract version this host speaks, re-exported for consumers.
pub const CONTRACT_VERSION: &str = grokptah_agent_sdk::CONTRACT_VERSION;
