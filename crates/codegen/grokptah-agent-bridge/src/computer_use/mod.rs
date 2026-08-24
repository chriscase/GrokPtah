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

mod coordination;
mod isolated_visual;
mod isolated_visual_artifacts;
mod isolated_visual_channel;
mod isolated_visual_frames;
mod isolated_visual_helper;
mod isolated_visual_input;
mod isolated_visual_input_wire;
mod isolated_visual_protocol;
#[cfg(target_os = "macos")]
mod macos_isolated_artifacts;
mod macos_observation;
mod platform;
mod policy;
mod projection;
mod reads;
mod service;
mod simulator;
mod store;
mod types;

pub use isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualLifecycle,
    IsolatedVisualLifecycleState, IsolatedVisualManifest, IsolatedVisualResourceLimits,
    IsolatedVisualSecurityProfile, IsolatedVisualTerminalDisposition,
    ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
    MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
pub use isolated_visual_artifacts::{
    measure_open_isolated_visual_artifact, measure_open_isolated_visual_artifacts,
    measure_packaged_isolated_visual_artifacts, IsolatedVisualArtifactMeasurement,
    IsolatedVisualArtifactMeasurements, IsolatedVisualArtifactRole,
    IsolatedVisualPackagedArtifactReceipt, ISOLATED_VISUAL_APP_BUNDLE_IDENTIFIER,
    ISOLATED_VISUAL_HELPER_SIGNING_IDENTIFIER, ISOLATED_VISUAL_MAX_CONFIGURATION_BYTES,
    ISOLATED_VISUAL_MAX_GUEST_IMAGE_BYTES, ISOLATED_VISUAL_MAX_HELPER_BYTES,
};
pub use isolated_visual_channel::{
    IsolatedVisualChannelBinding, ISOLATED_VISUAL_BINDING_CONTEXT,
    ISOLATED_VISUAL_BINDING_DIGEST_BYTES, ISOLATED_VISUAL_BINDING_HEADER_BYTES,
    ISOLATED_VISUAL_BINDING_MAGIC, ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES,
    ISOLATED_VISUAL_BINDING_TAG_BYTES, ISOLATED_VISUAL_BINDING_VERSION,
};
pub use isolated_visual_frames::{
    IsolatedVisualFrame, IsolatedVisualFrameCarrier, IsolatedVisualFrameChunk,
    ISOLATED_VISUAL_FRAME_CHUNK_BYTES, ISOLATED_VISUAL_FRAME_HEADER_BYTES,
    ISOLATED_VISUAL_FRAME_MAGIC, ISOLATED_VISUAL_FRAME_TAG_BYTES, ISOLATED_VISUAL_FRAME_VERSION,
};
pub use isolated_visual_helper::{
    IsolatedVisualHelperEvent, IsolatedVisualHelperEventCode, IsolatedVisualHelperFailure,
    IsolatedVisualHelperSupervisor, IsolatedVisualHelperSupervisorState,
    ISOLATED_VISUAL_HELPER_CONTROL_START, ISOLATED_VISUAL_HELPER_CONTROL_STOP,
    ISOLATED_VISUAL_HELPER_EVENT_BYTES, ISOLATED_VISUAL_HELPER_EVENT_MAGIC,
    ISOLATED_VISUAL_HELPER_EVENT_VERSION,
};
pub use isolated_visual_input::{
    IsolatedVisualInputGate, IsolatedVisualInputKeyState, IsolatedVisualInputMessage,
    ISOLATED_VISUAL_MAX_SCROLL_DELTA,
};
pub use isolated_visual_input_wire::{
    IsolatedVisualInputWire, ISOLATED_VISUAL_INPUT_HEADER_BYTES, ISOLATED_VISUAL_INPUT_MAGIC,
    ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES, ISOLATED_VISUAL_INPUT_TAG_BYTES,
    ISOLATED_VISUAL_INPUT_VERSION,
};
pub use isolated_visual_protocol::{
    IsolatedVisualGuestFailure, IsolatedVisualGuestHealth, IsolatedVisualGuestMessage,
    IsolatedVisualHostMessage, IsolatedVisualProtocolEnvelope, IsolatedVisualProtocolPayload,
    IsolatedVisualProtocolSession, IsolatedVisualProtocolSurfaceBinding,
    ISOLATED_VISUAL_CHANNEL_SECRET_BYTES, ISOLATED_VISUAL_MAX_SIGNED_ENVELOPE_BYTES,
};
pub use macos_observation::MacOsObservationPlatform;
pub use platform::{
    computer_isolated_visual_status, ComputerBackgroundSafetyReceipt,
    ComputerIsolatedVisualBlocker, ComputerIsolatedVisualStatus, ComputerObservationPlatform,
    ComputerPermission, ComputerPermissionStatus, ComputerPlatformStatus, ComputerTargetCandidate,
};
pub use policy::ComputerPolicy;
pub use projection::{
    project_run_at, ActionGrantSummary, ActionOutcomeSummary, ComputerBackendPublicView,
    ComputerErrorSummary, ComputerLocalApproval, ComputerLocalAuditEntry, ComputerLocalElement,
    ComputerLocalError, ComputerLocalGrant, ComputerLocalLimits, ComputerLocalObservation,
    ComputerLocalTarget, ComputerRunCapacity, ComputerRunEventPage, ComputerRunEventRange,
    ComputerRunProgress, ComputerRunProjection, ComputerScopeCapacity, ComputerSurfaceCoordination,
    ComputerSurfaceCoordinationState, ComputerSurfaceOccupant, ComputerTargetSummary,
    ComputerUncertainSurfaceLease, ObservationSummary, DEFAULT_EVENT_PAGE, MAX_EVENT_PAGE,
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
pub(crate) use types::ResolvedAgentComputerRunAdmission;
pub use types::{
    macos_background_safe_capability_proof, macos_native_capability_proof,
    macos_native_physical_input_domain, ActionClass, ActionGrant, ActionOutcome,
    AgentComputerRunRequest, ComputerAction, ComputerAttentionPoint, ComputerAttentionTarget,
    ComputerAuditEntry, ComputerAuthorityToken, ComputerBackend, ComputerCapabilities,
    ComputerCapabilityProof, ComputerCapabilityTier, ComputerControlDisposition,
    ComputerEmergencyControlToken, ComputerError, ComputerErrorCode, ComputerKey,
    ComputerObservation, ComputerPrincipal, ComputerRun, ComputerRunState, ComputerSurfaceBinding,
    ComputerSurfaceEvent, ComputerTarget, ComputerUseLimits, ComputerWorkAttemptBinding,
    EvidenceRef, GrantIssuer, IsolationProofOrigin, ObservationAuthority, ObservationGeometry,
    PhysicalInputDomain, PointerButton, PointerButtonState, SemanticAction, SemanticElement,
    Sensitivity, SurfaceFreshnessFence, AGENT_PRINCIPAL_INTEGRATION_BLOCKER,
    COMPUTER_RECEIPT_SCHEMA_VERSION, COMPUTER_RUN_SCHEMA_VERSION,
    FOREGROUND_CONFLICT_DOMAIN_CAPACITY, MACOS_BACKGROUND_SAFE_BACKEND_ID,
    MACOS_INTERRUPTED_BACKEND_ID, MACOS_NATIVE_BACKEND_ID, SIMULATOR_BACKGROUND_BACKEND_ID,
    SIMULATOR_FOREGROUND_BACKEND_ID, SIMULATOR_ISOLATED_BACKEND_ID,
};
