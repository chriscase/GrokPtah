//! Provider-neutral Computer Use contracts and simulator (#267/#268).
//!
//! The safety kernel remains provider-neutral. Native adapters must implement
//! [`ComputerBackend`] and pass through the policy/state layer. The first macOS
//! adapter exposes local consented observation and bounded semantic actions.
//! Model proposals live above this module so provider behavior cannot weaken
//! the state machine or policy boundary; MCP mutations remain absent.
//!
//! [`projection`] derives the redaction-safe serialized view that the desktop
//! cockpit and any coordinator surface both consume, so the two cannot
//! disagree about run state, control disposition, epoch, or event range.
//! Which runs each surface may list is a separate gate: the cockpit is
//! session-scoped; coordinator reads take [`ComputerReadBinding`].

mod helper_authority;
mod macos_observation;
mod package_identity;
mod platform;
mod policy;
mod projection;
mod qualification;
mod reads;
mod service;
mod simulator;
mod store;
mod types;

pub use helper_authority::{
    CleanupReceipt, EffectDisposition, EffectReceipt, HelperCrashCut, HelperLease,
    HelperSupervisor, HelperWorld,
};
pub use macos_observation::MacOsObservationPlatform;
pub use package_identity::{
    documented_identity_json, versions_compatible, ComputerExecutorIdentity, EligibilityInput,
    ExecutorKind, PackageIdentity, PackagedEligibility, SigningClass, APP_BUNDLE_ID,
    APP_EXECUTABLE, APP_MINIMUM_OS, APP_PRODUCT_NAME, APP_VERSION, COMPUTER_USE_MINIMUM_OS,
    DEMO_TARGET_BUNDLE_ID, HELPER_BUNDLE_ID, HELPER_EXECUTABLE, HELPER_MINIMUM_OS,
    HELPER_NESTED_PATH, HELPER_PRODUCT_NAME, HELPER_VERSION, PACKAGE_AUTHORITY_EVIDENCE_SCHEMA,
    PACKAGE_IDENTITY_SCHEMA,
};
pub use platform::{
    ComputerObservationPlatform, ComputerPermission, ComputerPermissionStatus,
    ComputerPlatformStatus, ComputerTargetCandidate,
};
pub use policy::ComputerPolicy;
pub use projection::{
    project_run_at, ActionGrantSummary, ActionOutcomeSummary, ComputerErrorSummary,
    ComputerRunCapacity, ComputerRunEventPage, ComputerRunEventRange, ComputerRunProgress,
    ComputerRunProjection, ComputerScopeCapacity, ComputerTargetSummary, ObservationSummary,
    DEFAULT_EVENT_PAGE, MAX_EVENT_PAGE,
};
pub use qualification::{
    run_synthetic_oracle, AuthorityVerdict, EvidenceAssembly, PackageAuthorityEvidence,
    QualificationScenario, ScenarioResult, SyntheticOracleReport,
};
pub use reads::{ComputerReadBinding, ComputerRunReads};

/// Canonical string form of a workspace path for the durable Computer Run
/// binding. This is the same canonicalization the control plane applies to a
/// caller-claimed workspace, so binding equality is an exact string compare.
pub fn canonical_workspace_string(path: &std::path::Path) -> Option<String> {
    crate::orchestration::canonical_workspace(path)
        .ok()
        .map(|canonical| canonical.display().to_string())
}

#[cfg(target_os = "macos")]
mod macos_native;
pub use service::ComputerUseService;
pub use simulator::SimulatorBackend;
pub use store::ComputerStore;
pub use types::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerAuditEntry, ComputerBackend,
    ComputerCapabilities, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerObservation, ComputerRun, ComputerRunState, ComputerTarget, ComputerUseLimits,
    EvidenceRef, GrantIssuer, ObservationGeometry, PointerButton, SemanticAction, SemanticElement,
    Sensitivity,
};
