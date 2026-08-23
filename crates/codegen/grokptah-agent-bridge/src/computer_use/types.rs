use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_ID_BYTES: usize = 256;
pub const MAX_LABEL_BYTES: usize = 512;
pub const MAX_TEXT_ENTRY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerErrorCode {
    InvalidRequest,
    InvalidState,
    Unauthorized,
    PermissionRequired,
    PermissionDenied,
    PermissionRevoked,
    UnsupportedPlatform,
    ForbiddenTarget,
    ForbiddenAction,
    SensitiveSurface,
    StaleObservation,
    TargetChanged,
    TargetClosed,
    LimitReached,
    Conflict,
    Pending,
    UncertainOutcome,
    Interrupted,
    BackendUnavailable,
    BackendFailure,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct ComputerError {
    pub code: ComputerErrorCode,
    pub message: String,
}

impl ComputerError {
    pub fn new(code: ComputerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: crate::textutil::truncate_at_char_boundary(&message.into(), 512).to_string(),
        }
    }
}

pub type ComputerResult<T> = Result<T, ComputerError>;

/// Durable Computer Run schema. `0` means a legacy record missing the field.
pub const COMPUTER_RUN_SCHEMA_VERSION: u32 = 1;
/// Durable Computer mutation receipt schema. `0` means a legacy unversioned receipt.
pub const COMPUTER_RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Public Agent-principal minting is intentionally unavailable. The only
/// issuance path resolves a durable `AgentRecord` and exact current spec
/// revision inside `AgentHost`; callers cannot authenticate with Agent-shaped
/// strings or deserialize a usable authority token.
pub const AGENT_PRINCIPAL_INTEGRATION_BLOCKER: &str = "Agent ComputerPrincipal minting requires a host-resolved durable AgentRecord and exact current spec revision";

/// Closed Computer Use isolation/capability tier.
///
/// This is the host-owned security classification. Public booleans on
/// [`ComputerCapabilities`] are a derived projection of a
/// [`ComputerCapabilityProof`] and never grant pointer, key, background, or
/// isolated authority by themselves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerCapabilityTier {
    /// May require activating the real foreground target. Current macOS native
    /// Computer Use is this tier and must not be advertised as isolated.
    ForegroundSemantic,
    /// Semantic actions that have been proven not to require foreground
    /// activation or pointer/key injection. ActivateTarget is forbidden.
    MeasuredBackgroundSafeSemantic,
    /// Independently isolated visual input domain. Pointer/key/visual actions
    /// require this proof. Stage 1 only the simulator can fixture this; a
    /// simulator fixture cannot make a native backend isolated.
    IndependentlyIsolatedVisualInputDomain,
    /// Missing, unknown, legacy, or unattested capability. Fail closed.
    #[default]
    Unproven,
}

impl ComputerCapabilityTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ForegroundSemantic => "foreground_semantic",
            Self::MeasuredBackgroundSafeSemantic => "measured_background_safe_semantic",
            Self::IndependentlyIsolatedVisualInputDomain => {
                "independently_isolated_visual_input_domain"
            }
            Self::Unproven => "unproven",
        }
    }

    pub fn allows_activate_target(self) -> bool {
        matches!(self, Self::ForegroundSemantic)
    }

    pub fn allows_background_semantic(self) -> bool {
        matches!(
            self,
            Self::ForegroundSemantic
                | Self::MeasuredBackgroundSafeSemantic
                | Self::IndependentlyIsolatedVisualInputDomain
        )
    }

    pub fn allows_isolated_input(self) -> bool {
        matches!(self, Self::IndependentlyIsolatedVisualInputDomain)
    }
}

/// Origin of an isolated-input proof. Simulator fixtures are explicitly not
/// native isolation and cannot attest a host helper that does not exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationProofOrigin {
    SimulatorFixture,
    HostNative,
}

/// Host-attested capability proof. This is the security boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComputerCapabilityProof {
    #[default]
    Unproven,
    ForegroundSemantic {
        backend_id: String,
        observe: bool,
        semantic_actions: bool,
        text_entry: bool,
    },
    MeasuredBackgroundSafeSemantic {
        backend_id: String,
        observe: bool,
        semantic_actions: bool,
        text_entry: bool,
        /// Host-issued measurement identity; never a model-provided string.
        measurement_id: String,
    },
    IndependentlyIsolatedVisualInputDomain {
        backend_id: String,
        surface_id: String,
        incarnation: String,
        input_domain_id: String,
        origin: IsolationProofOrigin,
        observe: bool,
        semantic_actions: bool,
        text_entry: bool,
        key_chords: bool,
        pointer_fallback: bool,
    },
}

impl ComputerCapabilityProof {
    pub fn tier(&self) -> ComputerCapabilityTier {
        match self {
            Self::Unproven => ComputerCapabilityTier::Unproven,
            Self::ForegroundSemantic { .. } => ComputerCapabilityTier::ForegroundSemantic,
            Self::MeasuredBackgroundSafeSemantic { .. } => {
                ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
            }
            Self::IndependentlyIsolatedVisualInputDomain { .. } => {
                ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain
            }
        }
    }

    pub fn backend_id(&self) -> &str {
        match self {
            Self::Unproven => "",
            Self::ForegroundSemantic { backend_id, .. }
            | Self::MeasuredBackgroundSafeSemantic { backend_id, .. }
            | Self::IndependentlyIsolatedVisualInputDomain { backend_id, .. } => backend_id,
        }
    }

    pub fn observe(&self) -> bool {
        match self {
            Self::Unproven => false,
            Self::ForegroundSemantic { observe, .. }
            | Self::MeasuredBackgroundSafeSemantic { observe, .. }
            | Self::IndependentlyIsolatedVisualInputDomain { observe, .. } => *observe,
        }
    }

    pub fn semantic_actions(&self) -> bool {
        match self {
            Self::Unproven => false,
            Self::ForegroundSemantic {
                semantic_actions, ..
            }
            | Self::MeasuredBackgroundSafeSemantic {
                semantic_actions, ..
            }
            | Self::IndependentlyIsolatedVisualInputDomain {
                semantic_actions, ..
            } => *semantic_actions,
        }
    }

    pub fn text_entry(&self) -> bool {
        match self {
            Self::Unproven => false,
            Self::ForegroundSemantic { text_entry, .. }
            | Self::MeasuredBackgroundSafeSemantic { text_entry, .. }
            | Self::IndependentlyIsolatedVisualInputDomain { text_entry, .. } => *text_entry,
        }
    }

    pub fn key_chords(&self) -> bool {
        match self {
            Self::IndependentlyIsolatedVisualInputDomain {
                origin: IsolationProofOrigin::SimulatorFixture,
                key_chords,
                ..
            } => *key_chords,
            _ => false,
        }
    }

    pub fn pointer_fallback(&self) -> bool {
        match self {
            Self::IndependentlyIsolatedVisualInputDomain {
                origin: IsolationProofOrigin::SimulatorFixture,
                pointer_fallback,
                ..
            } => *pointer_fallback,
            _ => false,
        }
    }

    pub fn isolation_origin(&self) -> Option<IsolationProofOrigin> {
        match self {
            Self::IndependentlyIsolatedVisualInputDomain { origin, .. } => Some(*origin),
            _ => None,
        }
    }

    pub fn isolated_surface(&self) -> Option<ComputerSurfaceBinding> {
        match self {
            Self::IndependentlyIsolatedVisualInputDomain {
                surface_id,
                incarnation,
                ..
            } => Some(ComputerSurfaceBinding {
                surface_id: surface_id.clone(),
                incarnation: incarnation.clone(),
            }),
            _ => None,
        }
    }

    pub fn input_domain_id(&self) -> Option<&str> {
        match self {
            Self::IndependentlyIsolatedVisualInputDomain {
                input_domain_id, ..
            } => Some(input_domain_id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn bind_to_interned_surface(
        self,
        surface: &ComputerSurfaceBinding,
        input_domain_id: &str,
        measurement_id: &str,
    ) -> ComputerResult<Self> {
        match self {
            Self::Unproven => Ok(Self::Unproven),
            Self::ForegroundSemantic { .. } => Ok(self),
            Self::MeasuredBackgroundSafeSemantic {
                backend_id,
                observe,
                semantic_actions,
                text_entry,
                ..
            } => Ok(Self::MeasuredBackgroundSafeSemantic {
                backend_id,
                observe,
                semantic_actions,
                text_entry,
                measurement_id: measurement_id.to_string(),
            }),
            Self::IndependentlyIsolatedVisualInputDomain {
                backend_id,
                origin,
                observe,
                semantic_actions,
                text_entry,
                key_chords,
                pointer_fallback,
                ..
            } => Ok(Self::IndependentlyIsolatedVisualInputDomain {
                backend_id,
                surface_id: surface.surface_id.clone(),
                incarnation: surface.incarnation.clone(),
                input_domain_id: input_domain_id.to_string(),
                origin,
                observe,
                semantic_actions,
                text_entry,
                key_chords,
                pointer_fallback,
            }),
        }
    }

    pub fn is_simulator_only_isolation(&self) -> bool {
        matches!(
            self,
            Self::IndependentlyIsolatedVisualInputDomain {
                origin: IsolationProofOrigin::SimulatorFixture,
                ..
            }
        )
    }

    /// Host-native isolated helpers are a later stage. A HostNative isolated
    /// proof is recorded honestly but cannot authorize dispatch in stage 1.
    pub fn isolated_input_is_dispatchable(&self) -> bool {
        matches!(
            self,
            Self::IndependentlyIsolatedVisualInputDomain {
                origin: IsolationProofOrigin::SimulatorFixture,
                ..
            }
        )
    }

    pub fn validate(&self) -> ComputerResult<()> {
        match self {
            Self::Unproven => Ok(()),
            Self::ForegroundSemantic { backend_id, .. } => {
                validate_id("backend_id", backend_id)?;
                if backend_id == SIMULATOR_ISOLATED_BACKEND_ID
                    || backend_id == SIMULATOR_BACKGROUND_BACKEND_ID
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::InvalidRequest,
                        "foreground-semantic proof cannot use a background or isolated backend id",
                    ));
                }
                Ok(())
            }
            Self::MeasuredBackgroundSafeSemantic {
                backend_id,
                measurement_id,
                ..
            } => {
                validate_id("backend_id", backend_id)?;
                validate_id("measurement_id", measurement_id)?;
                if is_native_macos_backend(backend_id) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "a native macOS backend cannot carry background-safe proof",
                    ));
                }
                Ok(())
            }
            Self::IndependentlyIsolatedVisualInputDomain {
                backend_id,
                surface_id,
                incarnation,
                input_domain_id,
                origin,
                ..
            } => {
                validate_id("backend_id", backend_id)?;
                ComputerSurfaceBinding {
                    surface_id: surface_id.clone(),
                    incarnation: incarnation.clone(),
                }
                .validate()?;
                validate_id("input_domain_id", input_domain_id)?;
                if is_native_macos_backend(backend_id) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "a native macOS backend cannot carry isolated-input proof",
                    ));
                }
                if *origin == IsolationProofOrigin::SimulatorFixture
                    && backend_id != SIMULATOR_ISOLATED_BACKEND_ID
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::InvalidRequest,
                        "simulator isolated proof must use the simulator-isolated backend id",
                    ));
                }
                if *origin == IsolationProofOrigin::HostNative {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "host-native isolated input is unsupported in isolation-contract stage 1",
                    ));
                }
                Ok(())
            }
        }
    }

    /// Legacy or unknown JSON cannot become background or isolated authority.
    pub fn from_wire(value: &serde_json::Value) -> Self {
        let Some(kind) = value.get("kind").and_then(|kind| kind.as_str()) else {
            return Self::Unproven;
        };
        let parsed = match kind {
            "unproven" | "unsupported" | "unsupported_unproven" => Ok(Self::Unproven),
            "foreground_semantic" => parse_foreground_proof(value),
            "measured_background_safe_semantic" => parse_background_proof(value),
            "independently_isolated_visual_input_domain" => parse_isolated_proof(value),
            _ => return Self::Unproven,
        };
        match parsed {
            Ok(proof) if proof.validate().is_ok() => proof,
            _ => Self::Unproven,
        }
    }
}

impl<'de> Deserialize<'de> for ComputerCapabilityProof {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self::from_wire(&value))
    }
}

fn json_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn parse_foreground_proof(value: &serde_json::Value) -> ComputerResult<ComputerCapabilityProof> {
    let backend_id = json_string(value, "backendId")
        .or_else(|| json_string(value, "backend_id"))
        .ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "missing backend_id")
        })?;
    if json_bool(value, "keyChords")
        || json_bool(value, "key_chords")
        || json_bool(value, "pointerFallback")
        || json_bool(value, "pointer_fallback")
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "foreground-semantic proof cannot claim pointer or key authority",
        ));
    }
    Ok(ComputerCapabilityProof::ForegroundSemantic {
        backend_id,
        observe: json_bool(value, "observe"),
        semantic_actions: json_bool(value, "semanticActions")
            || json_bool(value, "semantic_actions"),
        text_entry: json_bool(value, "textEntry") || json_bool(value, "text_entry"),
    })
}

fn parse_background_proof(value: &serde_json::Value) -> ComputerResult<ComputerCapabilityProof> {
    if json_bool(value, "keyChords")
        || json_bool(value, "key_chords")
        || json_bool(value, "pointerFallback")
        || json_bool(value, "pointer_fallback")
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "background-safe proof cannot claim pointer or key authority",
        ));
    }
    Ok(ComputerCapabilityProof::MeasuredBackgroundSafeSemantic {
        backend_id: json_string(value, "backendId")
            .or_else(|| json_string(value, "backend_id"))
            .ok_or_else(|| {
                ComputerError::new(ComputerErrorCode::InvalidRequest, "missing backend_id")
            })?,
        observe: json_bool(value, "observe"),
        semantic_actions: json_bool(value, "semanticActions")
            || json_bool(value, "semantic_actions"),
        text_entry: json_bool(value, "textEntry") || json_bool(value, "text_entry"),
        measurement_id: json_string(value, "measurementId")
            .or_else(|| json_string(value, "measurement_id"))
            .ok_or_else(|| {
                ComputerError::new(ComputerErrorCode::InvalidRequest, "missing measurement_id")
            })?,
    })
}

fn parse_isolated_proof(value: &serde_json::Value) -> ComputerResult<ComputerCapabilityProof> {
    let origin = match json_string(value, "origin").as_deref() {
        Some("simulator_fixture") => IsolationProofOrigin::SimulatorFixture,
        Some("host_native") => IsolationProofOrigin::HostNative,
        _ => {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated proof origin is missing or unknown",
            ))
        }
    };
    Ok(
        ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
            backend_id: json_string(value, "backendId")
                .or_else(|| json_string(value, "backend_id"))
                .ok_or_else(|| {
                    ComputerError::new(ComputerErrorCode::InvalidRequest, "missing backend_id")
                })?,
            surface_id: json_string(value, "surfaceId")
                .or_else(|| json_string(value, "surface_id"))
                .unwrap_or_default(),
            incarnation: json_string(value, "incarnation").unwrap_or_default(),
            input_domain_id: json_string(value, "inputDomainId")
                .or_else(|| json_string(value, "input_domain_id"))
                .unwrap_or_default(),
            origin,
            observe: json_bool(value, "observe"),
            semantic_actions: json_bool(value, "semanticActions")
                || json_bool(value, "semantic_actions"),
            text_entry: json_bool(value, "textEntry") || json_bool(value, "text_entry"),
            key_chords: json_bool(value, "keyChords") || json_bool(value, "key_chords"),
            pointer_fallback: json_bool(value, "pointerFallback")
                || json_bool(value, "pointer_fallback"),
        },
    )
}

pub const MACOS_NATIVE_BACKEND_ID: &str = "macos_accessibility_semantic";
pub const MACOS_INTERRUPTED_BACKEND_ID: &str = "macos_interrupted";
const MACOS_NATIVE_BACKEND_IDS: &[&str] = &[MACOS_NATIVE_BACKEND_ID, MACOS_INTERRUPTED_BACKEND_ID];

fn is_native_macos_backend(backend_id: &str) -> bool {
    MACOS_NATIVE_BACKEND_IDS.contains(&backend_id)
}
pub const SIMULATOR_FOREGROUND_BACKEND_ID: &str = "deterministic_simulator";
pub const SIMULATOR_BACKGROUND_BACKEND_ID: &str = "deterministic_simulator_background_safe";
pub const SIMULATOR_ISOLATED_BACKEND_ID: &str = "deterministic_simulator_isolated";

/// Typed macOS native capability. Public booleans are a derived projection of
/// this foreground-semantic proof; they never grant pointer, key, background,
/// or isolated authority.
pub fn macos_native_capability_proof() -> ComputerCapabilityProof {
    ComputerCapabilityProof::ForegroundSemantic {
        backend_id: MACOS_NATIVE_BACKEND_ID.into(),
        observe: true,
        semantic_actions: true,
        text_entry: true,
    }
}

/// Stage-1 host-global foreground conflict-domain capacity. Native macOS and
/// every current backend report 1. This is not isolated-surface capacity and
/// is not ledger occupancy (`maxRunRecords`).
pub const FOREGROUND_CONFLICT_DOMAIN_CAPACITY: u32 = 1;

/// Singleton physical input domain for native macOS. Per-window, process, and
/// bundle IDs are target identity; they are not isolation and must not intern
/// separate conflict domains.
pub fn macos_native_physical_input_domain() -> PhysicalInputDomain {
    PhysicalInputDomain::attested("macos-native", "host-global-foreground")
        .expect("host-global-foreground is a valid attested domain")
}

/// Opaque attested physical input-domain identity. Backends supply a stable
/// fingerprint; the host hashes it and never projects native handles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhysicalInputDomain {
    key: String,
}

impl PhysicalInputDomain {
    pub fn attested(kind: &str, attested_fingerprint: &str) -> ComputerResult<Self> {
        validate_id("physical_input_domain_kind", kind)?;
        if attested_fingerprint.is_empty()
            || attested_fingerprint.len() > 4096
            || attested_fingerprint.contains('\0')
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "physical input-domain fingerprint is missing or malformed",
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update([0]);
        hasher.update(attested_fingerprint.as_bytes());
        Ok(Self {
            key: format!("{:x}", hasher.finalize()),
        })
    }

    pub fn as_key(&self) -> &str {
        &self.key
    }
}

/// Opaque host attestation for one compiled-in Computer backend.
///
/// The public `ComputerBackend` trait remains implementable by embedders, but
/// the public service constructor is deliberately unproven. Trusted
/// constructors are crate-private, so an arbitrary backend cannot turn a
/// self-selected backend id or physical-domain fingerprint into
/// background/isolated authority.
#[derive(Debug, Clone)]
pub(crate) struct ComputerBackendAttestation {
    trust: ComputerBackendTrust,
    physical_domain: PhysicalInputDomain,
}

#[derive(Debug, Clone)]
enum ComputerBackendTrust {
    Unproven,
    Trusted {
        backend_id: String,
        tier: ComputerCapabilityTier,
    },
}

impl ComputerBackendAttestation {
    /// Fail-closed attestation used by every downstream backend unless the
    /// GrokPtah host registry recognizes its compiled-in implementation.
    pub(crate) fn unproven() -> Self {
        Self {
            trust: ComputerBackendTrust::Unproven,
            // Unknown backends share the native foreground conflict domain.
            // They cannot manufacture parallel capacity by varying a string.
            physical_domain: macos_native_physical_input_domain(),
        }
    }

    pub(crate) fn trusted(
        backend_id: impl Into<String>,
        tier: ComputerCapabilityTier,
        physical_domain: PhysicalInputDomain,
    ) -> ComputerResult<Self> {
        let backend_id = backend_id.into();
        validate_id("backend_id", &backend_id)?;
        if tier == ComputerCapabilityTier::Unproven {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "trusted backend attestation requires a proven tier",
            ));
        }
        Ok(Self {
            trust: ComputerBackendTrust::Trusted { backend_id, tier },
            physical_domain,
        })
    }

    pub(crate) fn physical_domain(&self) -> &PhysicalInputDomain {
        &self.physical_domain
    }

    pub(crate) fn attest_capabilities(
        &self,
        mut claimed: ComputerCapabilities,
    ) -> ComputerResult<ComputerCapabilities> {
        match &self.trust {
            ComputerBackendTrust::Unproven => Ok(ComputerCapabilities::unproven("unproven")),
            ComputerBackendTrust::Trusted { backend_id, tier } => {
                claimed.hydrate_legacy();
                claimed.validate()?;
                if claimed.backend_id != *backend_id
                    || claimed.proof.backend_id() != backend_id
                    || claimed.tier != *tier
                    || claimed.proof.tier() != *tier
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "backend capability claim does not match its host attestation",
                    ));
                }
                Ok(claimed)
            }
        }
    }
}

/// Host-minted opaque surface identity plus incarnation. Native process and
/// window handles must never appear here. Prefixes are not proof of issuance;
/// only the host surface registry mints these identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComputerSurfaceBinding {
    #[serde(default)]
    pub(crate) surface_id: String,
    #[serde(default)]
    pub(crate) incarnation: String,
}

impl ComputerSurfaceBinding {
    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }

    pub(crate) fn issue() -> Self {
        Self {
            surface_id: Uuid::new_v4().to_string(),
            incarnation: Uuid::new_v4().to_string(),
        }
    }

    pub(crate) fn rotate_incarnation(&self) -> ComputerResult<Self> {
        validate_id("surface_id", &self.surface_id)?;
        Ok(Self {
            surface_id: self.surface_id.clone(),
            incarnation: Uuid::new_v4().to_string(),
        })
    }

    pub fn is_issued(&self) -> bool {
        self.validate().is_ok()
    }

    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("surface_id", &self.surface_id)?;
        validate_id("incarnation", &self.incarnation)?;
        Ok(())
    }
}

/// Surface-scoped monotonic freshness. A wall-clock timestamp is never
/// dispatch proof; `tick` is meaningful only for the live incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceFreshnessFence {
    #[serde(default)]
    pub surface_id: String,
    #[serde(default)]
    pub incarnation: String,
    #[serde(default)]
    pub tick: u64,
    #[serde(default)]
    pub wall_clock: Option<DateTime<Utc>>,
}

impl SurfaceFreshnessFence {
    pub fn validate_shape(&self) -> ComputerResult<()> {
        ComputerSurfaceBinding {
            surface_id: self.surface_id.clone(),
            incarnation: self.incarnation.clone(),
        }
        .validate()?;
        if self.tick == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "monotonic freshness tick is missing",
            ));
        }
        Ok(())
    }
}

/// Durable principal recorded on a Computer Run. Agent records exist so
/// malicious or legacy disk records can deserialize; [`Self::validate`] fails
/// closed for Agent identity until the out-of-allowlist host Agent registry
/// lands. The wrapper is opaque so other crates cannot mint self-issued
/// authority. The public issue path is
/// [`crate::host::AgentHostHandle::computer_operator_token`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComputerPrincipal {
    inner: ComputerPrincipalInner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ComputerPrincipalInner {
    LocalOperatorSession {
        #[serde(rename = "sessionId", alias = "session_id")]
        session_id: Uuid,
    },
    Agent {
        #[serde(rename = "agentId", alias = "agent_id")]
        agent_id: String,
        #[serde(rename = "specRevision", alias = "spec_revision")]
        spec_revision: u64,
    },
}

impl ComputerPrincipal {
    pub(crate) fn local_operator(session_id: Uuid) -> Self {
        Self {
            inner: ComputerPrincipalInner::LocalOperatorSession { session_id },
        }
    }

    /// Public Agent minting is fail-closed. See
    /// [`AGENT_PRINCIPAL_INTEGRATION_BLOCKER`].
    pub fn agent(_agent_id: impl Into<String>, _spec_revision: u64) -> ComputerResult<Self> {
        Err(ComputerError::new(
            ComputerErrorCode::Unauthorized,
            AGENT_PRINCIPAL_INTEGRATION_BLOCKER,
        ))
    }

    /// Crate-private issuance seam for the durable AgentHost/AgentRecord
    /// integration. The public string constructor remains fail-closed: only
    /// host code that has already resolved the exact durable Agent and spec
    /// revision can call this function.
    pub(crate) fn from_host_agent_record(
        agent_id: impl Into<String>,
        spec_revision: u64,
    ) -> ComputerResult<Self> {
        let agent_id = agent_id.into();
        validate_id("agent_id", &agent_id)?;
        if spec_revision == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "agent principal requires a non-zero durable spec revision",
            ));
        }
        Ok(Self {
            inner: ComputerPrincipalInner::Agent {
                agent_id,
                spec_revision,
            },
        })
    }

    pub fn session_id(&self) -> Option<Uuid> {
        match &self.inner {
            ComputerPrincipalInner::LocalOperatorSession { session_id } => Some(*session_id),
            ComputerPrincipalInner::Agent { .. } => None,
        }
    }

    pub(crate) fn agent_id(&self) -> Option<&str> {
        match &self.inner {
            ComputerPrincipalInner::Agent { agent_id, .. } => Some(agent_id.as_str()),
            ComputerPrincipalInner::LocalOperatorSession { .. } => None,
        }
    }

    pub(crate) fn agent_spec_revision(&self) -> Option<u64> {
        match &self.inner {
            ComputerPrincipalInner::Agent { spec_revision, .. } => Some(*spec_revision),
            ComputerPrincipalInner::LocalOperatorSession { .. } => None,
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        match &self.inner {
            ComputerPrincipalInner::LocalOperatorSession { session_id } => {
                if session_id.is_nil() {
                    return Err(ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "local operator session identity is missing",
                    ));
                }
                Ok(())
            }
            ComputerPrincipalInner::Agent {
                agent_id,
                spec_revision,
            } => {
                validate_id("agent_id", agent_id)?;
                if *spec_revision == 0 {
                    return Err(ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "agent principal is missing its durable spec revision",
                    ));
                }
                Ok(())
            }
        }
    }

    pub fn public_kind(&self) -> &'static str {
        match &self.inner {
            ComputerPrincipalInner::LocalOperatorSession { .. } => "local_operator_session",
            ComputerPrincipalInner::Agent { .. } => "agent",
        }
    }
}

/// Host-resolved caller identity. Fields are private so callers cannot mint
/// authority; [`crate::host::AgentHostHandle::computer_operator_token`] is the
/// public issue path. Raw `ComputerUseService` callers cannot construct a token
/// from an arbitrary session UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerAuthorityToken {
    principal: ComputerPrincipal,
}

impl ComputerAuthorityToken {
    pub(crate) fn from_host_principal(principal: ComputerPrincipal) -> ComputerResult<Self> {
        principal.validate()?;
        Ok(Self { principal })
    }

    pub(crate) fn local_operator(session_id: Uuid) -> ComputerResult<Self> {
        Self::from_host_principal(ComputerPrincipal::local_operator(session_id))
    }

    pub(crate) fn agent_from_host_record(
        agent_id: impl Into<String>,
        spec_revision: u64,
    ) -> ComputerResult<Self> {
        Self::from_host_principal(ComputerPrincipal::from_host_agent_record(
            agent_id,
            spec_revision,
        )?)
    }

    pub fn principal(&self) -> &ComputerPrincipal {
        &self.principal
    }
}

/// Observation-scoped isolation binding. Missing fields deserialize empty and
/// cannot authorize background, isolated, pointer, or key actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ObservationAuthority {
    #[serde(default)]
    pub surface: ComputerSurfaceBinding,
    #[serde(default)]
    pub frame_epoch: u64,
    #[serde(default)]
    pub target_generation: u64,
    #[serde(default)]
    pub authority_epoch: u64,
    #[serde(default)]
    pub control_epoch: u64,
    #[serde(default)]
    pub freshness: SurfaceFreshnessFence,
}

impl ObservationAuthority {
    pub fn bind(
        run: &ComputerRun,
        frame_epoch: u64,
        freshness: SurfaceFreshnessFence,
    ) -> ComputerResult<Self> {
        run.surface.validate()?;
        freshness.validate_shape()?;
        if freshness.surface_id != run.surface.surface_id
            || freshness.incarnation != run.surface.incarnation
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "freshness fence is not bound to the run surface incarnation",
            ));
        }
        Ok(Self {
            surface: run.surface.clone(),
            frame_epoch,
            target_generation: run.target.generation,
            authority_epoch: run.authority_epoch,
            control_epoch: run.control_epoch,
            freshness,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTarget {
    /// Stable platform identity such as a bundle ID or package identity.
    pub app_id: String,
    /// Opaque, adapter-issued window identity. Never an OS pointer/handle.
    pub window_id: String,
    /// Changes when an app/window identity is recycled or rebound.
    pub generation: u64,
    /// Non-sensitive application label only; never a document or window title.
    pub display_name: String,
    pub sensitivity: Sensitivity,
}

impl ComputerTarget {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("app_id", &self.app_id)?;
        validate_id("window_id", &self.window_id)?;
        validate_text("display_name", &self.display_name, MAX_LABEL_BYTES)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    None,
    Potential,
    Secure,
    SystemRestricted,
}

impl Sensitivity {
    pub fn is_hard_denied(self) -> bool {
        matches!(self, Self::Secure | Self::SystemRestricted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale_factor: f64,
}

impl ObservationGeometry {
    pub fn validate(&self) -> ComputerResult<()> {
        if !self.x.is_finite()
            || !self.y.is_finite()
            || !self.width.is_finite()
            || !self.height.is_finite()
            || !self.scale_factor.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
            || !(0.25..=8.0).contains(&self.scale_factor)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid observation geometry",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRef {
    pub content_sha256: String,
    pub media_type: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub redacted: bool,
    /// Ephemeral adapter token; never a host filesystem path.
    pub asset_id: String,
}

impl EvidenceRef {
    pub fn validate(&self, limits: &ComputerUseLimits) -> ComputerResult<()> {
        validate_id("asset_id", &self.asset_id)?;
        if self.content_sha256.len() != 64
            || !self.content_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !matches!(
                self.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
            || self.byte_len > limits.max_screenshot_bytes
            || self.width == 0
            || self.height == 0
            || self.width > limits.max_screenshot_dimension
            || self.height > limits.max_screenshot_dimension
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid or oversized evidence reference",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAction {
    Invoke,
    SetValue,
    Select,
    Scroll,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticElement {
    /// Ephemeral reference scoped to one observation.
    pub element_id: String,
    pub role: String,
    pub label: Option<String>,
    /// Adapters must omit secure values rather than mark them redacted here.
    pub value: Option<String>,
    pub bounds: Option<ObservationGeometry>,
    pub enabled: bool,
    pub focused: bool,
    pub sensitivity: Sensitivity,
    pub actions: BTreeSet<SemanticAction>,
}

impl SemanticElement {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("element_id", &self.element_id)?;
        validate_text("role", &self.role, 128)?;
        if let Some(label) = &self.label {
            validate_text("label", label, MAX_LABEL_BYTES)?;
        }
        if let Some(value) = &self.value {
            if self.sensitivity.is_hard_denied() {
                return Err(ComputerError::new(
                    ComputerErrorCode::SensitiveSurface,
                    "secure elements must not expose values",
                ));
            }
            validate_text("value", value, MAX_LABEL_BYTES)?;
        }
        if let Some(bounds) = self.bounds {
            bounds.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerObservation {
    pub observation_id: String,
    pub sequence: u64,
    pub target: ComputerTarget,
    pub captured_at: DateTime<Utc>,
    pub geometry: ObservationGeometry,
    pub screenshot: Option<EvidenceRef>,
    pub elements: Vec<SemanticElement>,
    pub elements_truncated: bool,
    pub sensitivity: Sensitivity,
    /// Host-issued isolation binding. Missing/legacy fields are empty and
    /// cannot authorize background, isolated, pointer, or key actions.
    #[serde(default)]
    pub authority: ObservationAuthority,
}

impl ComputerObservation {
    pub fn validate(&self, limits: &ComputerUseLimits) -> ComputerResult<()> {
        validate_id("observation_id", &self.observation_id)?;
        self.target.validate()?;
        self.geometry.validate()?;
        if self.authority.surface.is_issued() {
            self.authority.surface.validate()?;
            self.authority.freshness.validate_shape()?;
        } else if self.authority.frame_epoch != 0
            || self.authority.authority_epoch != 0
            || self.authority.freshness.tick != 0
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "observation authority is malformed without a host-issued surface",
            ));
        }
        if self.elements.len() > limits.max_semantic_elements as usize {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "semantic element limit exceeded",
            ));
        }
        if let Some(screenshot) = &self.screenshot {
            screenshot.validate(limits)?;
        }
        let mut semantic_bytes = 0_u64;
        let mut ids = BTreeSet::new();
        for element in &self.elements {
            element.validate()?;
            if !ids.insert(&element.element_id) {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    "duplicate semantic element id",
                ));
            }
            semantic_bytes = semantic_bytes.saturating_add(
                serde_json::to_vec(element)
                    .map(|value| value.len() as u64)
                    .unwrap_or(u64::MAX),
            );
        }
        if semantic_bytes > limits.max_semantic_bytes {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "semantic observation byte limit exceeded",
            ));
        }
        Ok(())
    }

    pub fn element(&self, element_id: &str) -> Option<&SemanticElement> {
        self.elements
            .iter()
            .find(|element| element.element_id == element_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Semantic,
    TextEntry,
    KeyChord,
    PointerFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantIssuer {
    /// Explicit authorization made through the local GrokPtah operator UI.
    LocalUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerKey {
    Enter,
    Escape,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Space,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Shift,
    Control,
    Alt,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComputerAction {
    ActivateTarget,
    Invoke {
        element_id: String,
    },
    SetValue {
        element_id: String,
        text: String,
    },
    Select {
        element_id: String,
    },
    Scroll {
        element_id: Option<String>,
        delta_x: i32,
        delta_y: i32,
    },
    KeyChord {
        keys: Vec<ComputerKey>,
    },
    PointerClick {
        /// Target-relative logical coordinates, never global screen coordinates.
        x: f64,
        y: f64,
        button: PointerButton,
    },
    Wait {
        millis: u64,
    },
}

impl ComputerAction {
    pub fn class(&self) -> ActionClass {
        match self {
            Self::ActivateTarget
            | Self::Invoke { .. }
            | Self::Select { .. }
            | Self::Scroll { .. } => ActionClass::Semantic,
            Self::SetValue { .. } => ActionClass::TextEntry,
            Self::KeyChord { .. } => ActionClass::KeyChord,
            Self::PointerClick { .. } => ActionClass::PointerFallback,
            Self::Wait { .. } => ActionClass::Semantic,
        }
    }

    /// Isolation tier required to dispatch this action. Pointer/key never
    /// fall back to semantic or boolean capability flags.
    pub fn required_tier(&self) -> ComputerCapabilityTier {
        match self {
            Self::ActivateTarget => ComputerCapabilityTier::ForegroundSemantic,
            Self::KeyChord { .. } | Self::PointerClick { .. } => {
                ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain
            }
            Self::Invoke { .. }
            | Self::SetValue { .. }
            | Self::Select { .. }
            | Self::Scroll { .. }
            | Self::Wait { .. } => ComputerCapabilityTier::MeasuredBackgroundSafeSemantic,
        }
    }

    pub fn requires_isolated_input(&self) -> bool {
        matches!(self, Self::KeyChord { .. } | Self::PointerClick { .. })
    }

    pub fn is_activate_target(&self) -> bool {
        matches!(self, Self::ActivateTarget)
    }

    pub fn referenced_element(&self) -> Option<&str> {
        match self {
            Self::Invoke { element_id }
            | Self::SetValue { element_id, .. }
            | Self::Select { element_id } => Some(element_id),
            Self::Scroll { element_id, .. } => element_id.as_deref(),
            _ => None,
        }
    }

    pub fn validate(&self, limits: &ComputerUseLimits) -> ComputerResult<()> {
        if let Some(element_id) = self.referenced_element() {
            validate_id("element_id", element_id)?;
        }
        match self {
            Self::SetValue { text, .. } if text.len() > limits.max_text_entry_bytes as usize => {
                Err(ComputerError::new(
                    ComputerErrorCode::LimitReached,
                    "text entry limit exceeded",
                ))
            }
            Self::SetValue { text, .. } if text.contains('\0') => Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "text entry contains a null byte",
            )),
            Self::Scroll {
                delta_x, delta_y, ..
            } if delta_x.unsigned_abs() > 10_000 || delta_y.unsigned_abs() > 10_000 => {
                Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    "scroll delta exceeds the per-action bound",
                ))
            }
            Self::KeyChord { keys } if keys.is_empty() || keys.len() > 4 => Err(
                ComputerError::new(ComputerErrorCode::InvalidRequest, "invalid key chord"),
            ),
            Self::PointerClick { x, y, .. } if !x.is_finite() || !y.is_finite() => Err(
                ComputerError::new(ComputerErrorCode::InvalidRequest, "invalid pointer point"),
            ),
            Self::Wait { millis } if *millis > limits.max_wait_millis => Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "wait limit exceeded",
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionOutcome {
    pub summary: String,
    pub expected_postcondition_met: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerAuditEntry {
    pub sequence: u64,
    pub at: DateTime<Utc>,
    pub operation: String,
    pub disposition: String,
    pub action_class: Option<ActionClass>,
    pub observation_id: Option<String>,
    pub error_code: Option<ComputerErrorCode>,
}

impl ActionOutcome {
    pub fn bounded(summary: impl Into<String>, expected_postcondition_met: Option<bool>) -> Self {
        Self {
            summary: crate::textutil::truncate_at_char_boundary(&summary.into(), 512).to_string(),
            expected_postcondition_met,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerCapabilities {
    pub backend_id: String,
    pub observe: bool,
    pub semantic_actions: bool,
    pub text_entry: bool,
    pub key_chords: bool,
    pub pointer_fallback: bool,
    /// Typed proof is the security boundary. Booleans above are a public
    /// projection derived from this proof.
    #[serde(default)]
    pub proof: ComputerCapabilityProof,
    #[serde(default)]
    pub tier: ComputerCapabilityTier,
}

impl ComputerCapabilities {
    pub fn from_proof(proof: ComputerCapabilityProof) -> ComputerResult<Self> {
        proof.validate()?;
        let backend_id = match &proof {
            ComputerCapabilityProof::Unproven => "unproven".to_string(),
            _ => proof.backend_id().to_string(),
        };
        validate_id("backend_id", &backend_id)?;
        let caps = Self {
            backend_id,
            observe: proof.observe(),
            semantic_actions: proof.semantic_actions(),
            text_entry: proof.text_entry(),
            key_chords: proof.key_chords(),
            pointer_fallback: proof.pointer_fallback(),
            tier: proof.tier(),
            proof,
        };
        caps.validate()?;
        Ok(caps)
    }

    pub fn unproven(backend_id: impl Into<String>) -> Self {
        let backend_id = backend_id.into();
        Self {
            backend_id,
            observe: false,
            semantic_actions: false,
            text_entry: false,
            key_chords: false,
            pointer_fallback: false,
            proof: ComputerCapabilityProof::Unproven,
            tier: ComputerCapabilityTier::Unproven,
        }
    }

    /// Repair missing typed fields from legacy boolean-only JSON. Never
    /// upgrades pointer/key/background/isolated authority.
    pub fn hydrate_legacy(&mut self) {
        let proof_missing = matches!(self.proof, ComputerCapabilityProof::Unproven);
        let tier_missing = matches!(self.tier, ComputerCapabilityTier::Unproven);
        if !proof_missing || !tier_missing {
            return;
        }
        let claimed_pointer_or_key = self.key_chords || self.pointer_fallback;
        self.key_chords = false;
        self.pointer_fallback = false;
        if claimed_pointer_or_key {
            self.proof = ComputerCapabilityProof::Unproven;
            self.tier = ComputerCapabilityTier::Unproven;
            return;
        }
        if self.observe || self.semantic_actions || self.text_entry {
            if self.backend_id.is_empty() {
                return;
            }
            self.proof = ComputerCapabilityProof::ForegroundSemantic {
                backend_id: self.backend_id.clone(),
                observe: self.observe,
                semantic_actions: self.semantic_actions,
                text_entry: self.text_entry,
            };
            self.tier = ComputerCapabilityTier::ForegroundSemantic;
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        if matches!(self.proof, ComputerCapabilityProof::Unproven)
            && matches!(self.tier, ComputerCapabilityTier::Unproven)
        {
            if self.key_chords || self.pointer_fallback {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    "unproven capability cannot project pointer or key authority",
                ));
            }
            return Ok(());
        }
        self.proof.validate()?;
        let projected = Self {
            backend_id: if self.proof.backend_id().is_empty() {
                self.backend_id.clone()
            } else {
                self.proof.backend_id().to_string()
            },
            observe: self.proof.observe(),
            semantic_actions: self.proof.semantic_actions(),
            text_entry: self.proof.text_entry(),
            key_chords: self.proof.key_chords(),
            pointer_fallback: self.proof.pointer_fallback(),
            proof: self.proof.clone(),
            tier: self.proof.tier(),
        };
        if self.tier != projected.tier
            || self.observe != projected.observe
            || self.semantic_actions != projected.semantic_actions
            || self.text_entry != projected.text_entry
            || self.key_chords != projected.key_chords
            || self.pointer_fallback != projected.pointer_fallback
            || (!self.proof.backend_id().is_empty() && self.backend_id != projected.backend_id)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "capability tier, proof, and boolean projection contradict each other",
            ));
        }
        Ok(())
    }

    pub fn allows_action(&self, action: &ComputerAction) -> bool {
        if self.validate().is_err() {
            return false;
        }
        if action.requires_isolated_input() {
            return self.proof.isolated_input_is_dispatchable()
                && match action {
                    ComputerAction::KeyChord { .. } => self.proof.key_chords(),
                    ComputerAction::PointerClick { .. } => self.proof.pointer_fallback(),
                    _ => false,
                };
        }
        if action.is_activate_target() {
            return self.tier.allows_activate_target() && self.proof.semantic_actions();
        }
        match action.class() {
            ActionClass::Semantic => {
                self.tier.allows_background_semantic() && self.proof.semantic_actions()
            }
            ActionClass::TextEntry => {
                self.tier.allows_background_semantic() && self.proof.text_entry()
            }
            ActionClass::KeyChord | ActionClass::PointerFallback => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerRunState {
    AwaitingAuthorization,
    Ready,
    Observing,
    Acting,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    LimitReached,
}

/// Durable operator-control disposition layered on top of the lifecycle state.
///
/// `Paused` is intentionally not enough to describe ownership: a paused run
/// may be resumed by a fresh local grant, while an operator takeover must not
/// be revived by a stale approval or reconnect.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerControlDisposition {
    #[default]
    AgentOwned,
    Paused,
    OperatorTakeover,
    Stopped,
    Interrupted,
    UncertainOutcome,
}

impl ComputerRunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::LimitReached
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseLimits {
    pub max_actions: u32,
    pub max_duration_secs: u64,
    pub max_retries_per_action: u32,
    pub max_observation_age_millis: u64,
    pub max_screenshot_bytes: u64,
    pub max_screenshot_dimension: u32,
    pub max_semantic_elements: u32,
    pub max_semantic_bytes: u64,
    pub max_evidence_bytes: u64,
    pub max_text_entry_bytes: u32,
    pub max_wait_millis: u64,
}

impl Default for ComputerUseLimits {
    fn default() -> Self {
        Self {
            max_actions: 64,
            max_duration_secs: 15 * 60,
            max_retries_per_action: 2,
            max_observation_age_millis: 10_000,
            max_screenshot_bytes: 4 * 1024 * 1024,
            max_screenshot_dimension: 8_192,
            max_semantic_elements: 2_000,
            max_semantic_bytes: 1024 * 1024,
            max_evidence_bytes: 64 * 1024 * 1024,
            max_text_entry_bytes: MAX_TEXT_ENTRY_BYTES as u32,
            max_wait_millis: 5_000,
        }
    }
}

impl ComputerUseLimits {
    pub fn ceiling() -> Self {
        Self {
            max_actions: 256,
            max_duration_secs: 60 * 60,
            max_retries_per_action: 5,
            max_observation_age_millis: 60_000,
            max_screenshot_bytes: 16 * 1024 * 1024,
            max_screenshot_dimension: 16_384,
            max_semantic_elements: 10_000,
            max_semantic_bytes: 8 * 1024 * 1024,
            max_evidence_bytes: 256 * 1024 * 1024,
            max_text_entry_bytes: MAX_TEXT_ENTRY_BYTES as u32,
            max_wait_millis: 30_000,
        }
    }

    pub fn validate(self) -> ComputerResult<Self> {
        let ceiling = Self::ceiling();
        let valid = self.max_actions > 0
            && self.max_actions <= ceiling.max_actions
            && self.max_duration_secs > 0
            && self.max_duration_secs <= ceiling.max_duration_secs
            && self.max_retries_per_action <= ceiling.max_retries_per_action
            && self.max_observation_age_millis > 0
            && self.max_observation_age_millis <= ceiling.max_observation_age_millis
            && self.max_screenshot_bytes > 0
            && self.max_screenshot_bytes <= ceiling.max_screenshot_bytes
            && self.max_screenshot_dimension > 0
            && self.max_screenshot_dimension <= ceiling.max_screenshot_dimension
            && self.max_semantic_elements > 0
            && self.max_semantic_elements <= ceiling.max_semantic_elements
            && self.max_semantic_bytes > 0
            && self.max_semantic_bytes <= ceiling.max_semantic_bytes
            && self.max_evidence_bytes > 0
            && self.max_evidence_bytes <= ceiling.max_evidence_bytes
            && self.max_text_entry_bytes > 0
            && self.max_text_entry_bytes <= ceiling.max_text_entry_bytes
            && self.max_wait_millis > 0
            && self.max_wait_millis <= ceiling.max_wait_millis;
        if !valid {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "computer-use limits exceed a hard ceiling or contain zero",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionGrant {
    pub grant_id: String,
    pub run_id: String,
    pub target: ComputerTarget,
    pub action_classes: BTreeSet<ActionClass>,
    pub issued_by: GrantIssuer,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub uses_remaining: Option<u32>,
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub surface: ComputerSurfaceBinding,
    #[serde(default)]
    pub authority_epoch: u64,
    #[serde(default)]
    pub principal: Option<ComputerPrincipal>,
    #[serde(default)]
    pub capability_tier: ComputerCapabilityTier,
}

/// Durable WorkAttempt identity for an Agent-owned Computer Run. The host
/// freezes this binding at Run admission; the Computer backend and model never
/// author or widen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerWorkAttemptBinding {
    pub work_id: String,
    pub work_attempt_id: String,
    pub agent_id: String,
    pub agent_spec_revision: u64,
}

impl ComputerWorkAttemptBinding {
    pub(crate) fn validate(&self) -> ComputerResult<()> {
        validate_id("work_id", &self.work_id)?;
        validate_id("work_attempt_id", &self.work_attempt_id)?;
        validate_id("agent_id", &self.agent_id)?;
        if self.agent_spec_revision == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "Computer WorkAttempt binding is missing its Agent spec revision",
            ));
        }
        Ok(())
    }
}

/// Request to bind one already-active durable WorkAttempt to a Computer Run.
/// Session, workspace, Agent identity, and spec revision are deliberately
/// absent: `AgentHost` resolves those from durable records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentComputerRunRequest {
    pub request_id: String,
    pub work_id: String,
    pub work_attempt_id: String,
    pub target: ComputerTarget,
    pub limits: ComputerUseLimits,
}

impl AgentComputerRunRequest {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("request_id", &self.request_id)?;
        validate_id("work_id", &self.work_id)?;
        validate_id("work_attempt_id", &self.work_attempt_id)?;
        self.target.validate()?;
        self.limits.validate().map(|_| ())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAgentComputerRunAdmission {
    pub request_id: String,
    pub owner_session_id: Uuid,
    pub binding: ComputerWorkAttemptBinding,
    pub workspace: String,
    pub target: ComputerTarget,
    pub limits: ComputerUseLimits,
}

impl ActionGrant {
    pub fn for_run(
        run: &ComputerRun,
        action_classes: BTreeSet<ActionClass>,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        uses_remaining: Option<u32>,
    ) -> Self {
        Self {
            grant_id: Uuid::new_v4().to_string(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            action_classes,
            issued_by: GrantIssuer::LocalUser,
            issued_at,
            expires_at,
            uses_remaining,
            revoked_at: None,
            surface: run.surface.clone(),
            authority_epoch: run.authority_epoch,
            principal: run.initiating_principal.clone(),
            capability_tier: run.capability_proof.tier(),
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("grant_id", &self.grant_id)?;
        validate_id("run_id", &self.run_id)?;
        self.target.validate()?;
        if self.action_classes.is_empty() || self.expires_at <= self.issued_at {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "grant must have action classes and a positive lifetime",
            ));
        }
        if self.uses_remaining == Some(0) && self.revoked_at.is_none() {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "grant has no remaining uses",
            ));
        }
        self.surface.validate()?;
        let Some(principal) = &self.principal else {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "grant is missing an initiating principal",
            ));
        };
        principal.validate()?;
        if matches!(self.capability_tier, ComputerCapabilityTier::Unproven) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven capability cannot issue a grant",
            ));
        }
        Ok(())
    }

    pub fn effective_tier(&self) -> ComputerCapabilityTier {
        self.capability_tier
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRun {
    pub run_id: String,
    pub owner_session_id: Uuid,
    /// Canonical workspace path bound at creation and preserved through
    /// restart recovery (#271). `None` on records created before the binding
    /// existed; workspace-scoped MCP reads fail closed on `None` instead of
    /// inferring a workspace from current process state.
    #[serde(default)]
    pub workspace: Option<String>,
    pub parent_run_id: Option<String>,
    pub campaign_id: Option<String>,
    pub target: ComputerTarget,
    pub state: ComputerRunState,
    /// Durable ownership/control state exposed to GUI and MCP projections.
    #[serde(default)]
    pub control_disposition: ComputerControlDisposition,
    /// Monotonic fence incremented by pause, takeover, stop, and recovery.
    #[serde(default)]
    pub control_epoch: u64,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub limits: ComputerUseLimits,
    pub action_count: u32,
    pub evidence_bytes: u64,
    pub current_observation: Option<ComputerObservation>,
    pub grant: Option<ActionGrant>,
    pub last_outcome: Option<ActionOutcome>,
    pub audit: Vec<ComputerAuditEntry>,
    pub last_error: Option<ComputerError>,
    #[serde(default)]
    pub surface: ComputerSurfaceBinding,
    #[serde(default)]
    pub initiating_principal: Option<ComputerPrincipal>,
    #[serde(default)]
    pub authority_epoch: u64,
    #[serde(default)]
    pub capability_proof: ComputerCapabilityProof,
    #[serde(default)]
    pub freshness_tick: u64,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_attempt: Option<ComputerWorkAttemptBinding>,
}

impl ComputerRun {
    pub fn new(
        owner_session_id: Uuid,
        workspace: Option<String>,
        target: ComputerTarget,
        limits: ComputerUseLimits,
    ) -> ComputerResult<Self> {
        target.validate()?;
        let limits = limits.validate()?;
        validate_workspace(workspace.as_deref())?;
        let now = Utc::now();
        Ok(Self {
            run_id: Uuid::new_v4().to_string(),
            owner_session_id,
            workspace,
            parent_run_id: None,
            campaign_id: None,
            target,
            state: ComputerRunState::AwaitingAuthorization,
            control_disposition: ComputerControlDisposition::AgentOwned,
            control_epoch: 0,
            version: 1,
            created_at: now,
            updated_at: now,
            started_at: None,
            ended_at: None,
            limits,
            action_count: 0,
            evidence_bytes: 0,
            current_observation: None,
            grant: None,
            last_outcome: None,
            audit: Vec::new(),
            last_error: None,
            surface: ComputerSurfaceBinding::default(),
            initiating_principal: None,
            authority_epoch: 0,
            capability_proof: ComputerCapabilityProof::Unproven,
            freshness_tick: 0,
            schema_version: COMPUTER_RUN_SCHEMA_VERSION,
            work_attempt: None,
        })
    }

    pub(crate) fn new_with_isolation(
        owner_session_id: Uuid,
        workspace: Option<String>,
        target: ComputerTarget,
        limits: ComputerUseLimits,
        principal: ComputerPrincipal,
        surface: ComputerSurfaceBinding,
        proof: ComputerCapabilityProof,
    ) -> ComputerResult<Self> {
        principal.validate()?;
        if principal
            .session_id()
            .is_some_and(|session_id| session_id != owner_session_id)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "local operator principal does not match the owning session",
            ));
        }
        surface.validate()?;
        proof.validate()?;
        if is_native_macos_backend(proof.backend_id())
            && !matches!(proof, ComputerCapabilityProof::ForegroundSemantic { .. })
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "native macOS Computer Use can only be stamped foreground-semantic",
            ));
        }
        if let Some(isolated_surface) = proof.isolated_surface() {
            if isolated_surface != surface {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenTarget,
                    "isolated capability proof is not bound to this backend surface",
                ));
            }
        }
        if matches!(proof.tier(), ComputerCapabilityTier::Unproven) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven capability cannot create an attested computer run",
            ));
        }
        let mut run = Self::new(owner_session_id, workspace, target, limits)?;
        run.surface = surface;
        run.initiating_principal = Some(principal);
        run.capability_proof = proof;
        run.authority_epoch = 0;
        run.freshness_tick = 0;
        run.schema_version = COMPUTER_RUN_SCHEMA_VERSION;
        Ok(run)
    }

    #[cfg(test)]
    pub(crate) fn attested_foreground_for_test(
        owner_session_id: Uuid,
        workspace: Option<String>,
        target: ComputerTarget,
        limits: ComputerUseLimits,
    ) -> ComputerResult<Self> {
        let surface = ComputerSurfaceBinding::issue();
        Self::new_with_isolation(
            owner_session_id,
            workspace,
            target,
            limits,
            ComputerPrincipal::local_operator(owner_session_id),
            surface,
            ComputerCapabilityProof::ForegroundSemantic {
                backend_id: "test_foreground".into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
            },
        )
    }

    pub fn required_principal(&self) -> ComputerResult<&ComputerPrincipal> {
        let Some(principal) = self.initiating_principal.as_ref() else {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer run has no initiating principal",
            ));
        };
        principal.validate()?;
        Ok(principal)
    }

    pub fn bump_authority_epoch(&mut self) {
        self.authority_epoch = self.authority_epoch.saturating_add(1);
        self.updated_at = Utc::now();
    }

    pub fn record_audit(
        &mut self,
        operation: &str,
        disposition: &str,
        action_class: Option<ActionClass>,
        observation_id: Option<String>,
        error_code: Option<ComputerErrorCode>,
    ) {
        const MAX_AUDIT_ENTRIES: usize = 1_024;
        if self.audit.len() == MAX_AUDIT_ENTRIES {
            self.audit.remove(0);
        }
        self.audit.push(ComputerAuditEntry {
            sequence: self.audit.last().map_or(1, |entry| entry.sequence + 1),
            at: Utc::now(),
            operation: crate::textutil::truncate_at_char_boundary(operation, 64).to_string(),
            disposition: crate::textutil::truncate_at_char_boundary(disposition, 64).to_string(),
            action_class,
            observation_id: observation_id.map(|value| {
                crate::textutil::truncate_at_char_boundary(&value, MAX_ID_BYTES).to_string()
            }),
            error_code,
        });
    }

    pub fn set_control_disposition(&mut self, disposition: ComputerControlDisposition) {
        if self.control_disposition != disposition {
            self.control_disposition = disposition;
            self.control_epoch = self.control_epoch.saturating_add(1);
            self.updated_at = Utc::now();
        }
    }

    pub fn transition(&mut self, next: ComputerRunState) -> ComputerResult<()> {
        if self.state == next {
            return Ok(());
        }
        let legal = matches!(
            (self.state, next),
            (
                ComputerRunState::AwaitingAuthorization,
                ComputerRunState::Ready
            ) | (
                ComputerRunState::AwaitingAuthorization,
                ComputerRunState::Cancelled
            ) | (
                ComputerRunState::AwaitingAuthorization,
                ComputerRunState::Paused
            ) | (ComputerRunState::Ready, ComputerRunState::Observing)
                | (ComputerRunState::Ready, ComputerRunState::Acting)
                | (ComputerRunState::Ready, ComputerRunState::Paused)
                | (ComputerRunState::Ready, ComputerRunState::Completed)
                | (ComputerRunState::Ready, ComputerRunState::Cancelled)
                | (ComputerRunState::Ready, ComputerRunState::LimitReached)
                | (ComputerRunState::Observing, ComputerRunState::Ready)
                | (ComputerRunState::Observing, ComputerRunState::Paused)
                | (ComputerRunState::Observing, ComputerRunState::Failed)
                | (ComputerRunState::Observing, ComputerRunState::Cancelled)
                | (ComputerRunState::Observing, ComputerRunState::LimitReached)
                | (ComputerRunState::Acting, ComputerRunState::Ready)
                | (ComputerRunState::Acting, ComputerRunState::Paused)
                | (ComputerRunState::Acting, ComputerRunState::Failed)
                | (ComputerRunState::Acting, ComputerRunState::Cancelled)
                | (ComputerRunState::Acting, ComputerRunState::LimitReached)
                | (ComputerRunState::Paused, ComputerRunState::Ready)
                | (ComputerRunState::Paused, ComputerRunState::Cancelled)
                | (_, ComputerRunState::Interrupted)
        );
        if self.state.is_terminal() || !legal {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                format!(
                    "illegal computer run transition {:?} -> {next:?}",
                    self.state
                ),
            ));
        }
        let now = Utc::now();
        self.state = next;
        self.version = self.version.saturating_add(1);
        self.updated_at = now;
        if self.started_at.is_none() && next == ComputerRunState::Ready {
            self.started_at = Some(now);
        }
        if next.is_terminal() {
            self.ended_at = Some(now);
        }
        Ok(())
    }

    pub fn duration_exceeded(&self, now: DateTime<Utc>) -> bool {
        self.started_at.is_some_and(|started| {
            now.signed_duration_since(started)
                > Duration::seconds(self.limits.max_duration_secs as i64)
        })
    }
}

#[async_trait]
pub trait ComputerBackend: Send + Sync + std::fmt::Debug {
    fn capabilities(&self) -> ComputerCapabilities;

    /// Legacy/advisory backend domain. The service never uses this value as
    /// authority; only the opaque host attestation supplies the conflict
    /// domain that is interned into a surface.
    fn physical_input_domain(&self) -> PhysicalInputDomain;

    async fn observe(
        &self,
        run_id: &str,
        observation_id: &str,
        target: &ComputerTarget,
        limits: &ComputerUseLimits,
    ) -> ComputerResult<ComputerObservation>;

    async fn act(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome>;

    /// Backend-attested atomic action: compare the live native AX/tree/frame
    /// generation and surface incarnation to the exact observed attestation
    /// under the same dispatch lock immediately before input. Unsupported
    /// backends must deny without performing input.
    async fn act_if_current(
        &self,
        _run_id: &str,
        _observation: &ComputerObservation,
        _action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        Err(ComputerError::new(
            ComputerErrorCode::ForbiddenAction,
            "backend does not attest exact-current action",
        ))
    }

    /// Returns only process-owned evidence for the exact run and opaque asset ID.
    async fn read_evidence(
        &self,
        _run_id: &str,
        _asset_id: &str,
    ) -> ComputerResult<Option<Vec<u8>>> {
        Ok(None)
    }

    async fn cancel(&self, run_id: &str) -> ComputerResult<()>;
}

pub(super) fn validate_id(name: &str, value: &str) -> ComputerResult<()> {
    if value.trim().is_empty()
        || value.len() > MAX_ID_BYTES
        || value.contains('\0')
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("invalid {name}"),
        ));
    }
    Ok(())
}

/// A durable workspace binding must be a plausible canonical path string; it
/// is compared for exact equality, never interpreted, so only shape is
/// validated here.
pub(super) fn validate_workspace(workspace: Option<&str>) -> ComputerResult<()> {
    if let Some(workspace) = workspace {
        if workspace.trim().is_empty() || workspace.len() > 4096 || workspace.contains('\0') {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid workspace binding",
            ));
        }
    }
    Ok(())
}

fn validate_text(name: &str, value: &str, max: usize) -> ComputerResult<()> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("invalid {name}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    #[test]
    fn hard_ceilings_reject_escalation() {
        let limits = ComputerUseLimits {
            max_actions: ComputerUseLimits::ceiling().max_actions + 1,
            ..Default::default()
        };
        assert_eq!(
            limits.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn state_machine_never_leaves_terminal_state() {
        let mut run = ComputerRun::attested_foreground_for_test(
            Uuid::new_v4(),
            None,
            target(),
            Default::default(),
        )
        .unwrap();
        run.transition(ComputerRunState::Ready).unwrap();
        run.transition(ComputerRunState::Cancelled).unwrap();
        assert!(run.transition(ComputerRunState::Ready).is_err());
    }

    #[test]
    fn secure_element_cannot_carry_value() {
        let element = SemanticElement {
            element_id: "password".into(),
            role: "secure_text_field".into(),
            label: Some("Password".into()),
            value: Some("secret".into()),
            bounds: None,
            enabled: true,
            focused: false,
            sensitivity: Sensitivity::Secure,
            actions: BTreeSet::new(),
        };
        assert_eq!(
            element.validate().unwrap_err().code,
            ComputerErrorCode::SensitiveSurface
        );
    }

    #[test]
    fn action_payloads_have_per_action_bounds() {
        let limits = ComputerUseLimits::default();
        assert_eq!(
            ComputerAction::SetValue {
                element_id: "field".into(),
                text: "not\0valid".into(),
            }
            .validate(&limits)
            .unwrap_err()
            .code,
            ComputerErrorCode::InvalidRequest
        );
        assert_eq!(
            ComputerAction::Scroll {
                element_id: None,
                delta_x: 0,
                delta_y: 10_001,
            }
            .validate(&limits)
            .unwrap_err()
            .code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn legacy_boolean_capabilities_hydrate_to_foreground_or_unproven() {
        let mut semantic: ComputerCapabilities = serde_json::from_value(serde_json::json!({
            "backendId": "legacy_semantic",
            "observe": true,
            "semanticActions": true,
            "textEntry": true,
            "keyChords": false,
            "pointerFallback": false
        }))
        .unwrap();
        assert_eq!(semantic.proof, ComputerCapabilityProof::Unproven);
        semantic.hydrate_legacy();
        semantic.validate().unwrap();
        assert_eq!(semantic.tier, ComputerCapabilityTier::ForegroundSemantic);
        assert!(semantic.allows_action(&ComputerAction::ActivateTarget));
        assert!(semantic.allows_action(&ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into(),
        }));
        assert!(!semantic.allows_action(&ComputerAction::PointerClick {
            x: 1.0,
            y: 1.0,
            button: PointerButton::Primary,
        }));
        assert!(!semantic.allows_action(&ComputerAction::KeyChord {
            keys: vec![ComputerKey::Enter],
        }));

        let mut pointer: ComputerCapabilities = serde_json::from_value(serde_json::json!({
            "backendId": "legacy_pointer",
            "observe": true,
            "semanticActions": true,
            "textEntry": true,
            "keyChords": true,
            "pointerFallback": true
        }))
        .unwrap();
        pointer.hydrate_legacy();
        pointer.validate().unwrap();
        assert_eq!(pointer.tier, ComputerCapabilityTier::Unproven);
        assert!(!pointer.pointer_fallback);
        assert!(!pointer.key_chords);
        assert!(!pointer.allows_action(&ComputerAction::PointerClick {
            x: 1.0,
            y: 1.0,
            button: PointerButton::Primary,
        }));
        assert!(!pointer.allows_action(&ComputerAction::ActivateTarget));
    }

    #[test]
    fn contradictory_tier_proof_and_booleans_fail_closed() {
        let mut caps =
            ComputerCapabilities::from_proof(ComputerCapabilityProof::ForegroundSemantic {
                backend_id: "fg".into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
            })
            .unwrap();
        caps.tier = ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain;
        assert_eq!(
            caps.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
        caps.tier = ComputerCapabilityTier::ForegroundSemantic;
        caps.pointer_fallback = true;
        assert_eq!(
            caps.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
        caps.pointer_fallback = false;
        caps.proof = ComputerCapabilityProof::MeasuredBackgroundSafeSemantic {
            backend_id: SIMULATOR_BACKGROUND_BACKEND_ID.into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            measurement_id: format!("measurement-{}", Uuid::new_v4()),
        };
        assert_eq!(
            caps.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn unknown_malformed_and_host_native_isolated_proofs_deserialize_unproven() {
        let unknown: ComputerCapabilityProof = serde_json::from_value(serde_json::json!({
            "kind": "totally_new_tier",
            "pointerFallback": true
        }))
        .unwrap();
        assert_eq!(unknown, ComputerCapabilityProof::Unproven);

        let missing: ComputerCapabilityProof = serde_json::from_value(serde_json::json!({
            "backendId": "x"
        }))
        .unwrap();
        assert_eq!(missing, ComputerCapabilityProof::Unproven);

        let host_native: ComputerCapabilityProof = serde_json::from_value(serde_json::json!({
            "kind": "independently_isolated_visual_input_domain",
            "backendId": MACOS_NATIVE_BACKEND_ID,
            "surfaceId": format!("surface-{}", Uuid::new_v4()),
            "incarnation": format!("incarnation-{}", Uuid::new_v4()),
            "inputDomainId": format!("input-domain-{}", Uuid::new_v4()),
            "origin": "host_native",
            "observe": true,
            "semanticActions": true,
            "textEntry": true,
            "keyChords": true,
            "pointerFallback": true
        }))
        .unwrap();
        assert_eq!(host_native, ComputerCapabilityProof::Unproven);

        let native_simulator_origin: ComputerCapabilityProof =
            serde_json::from_value(serde_json::json!({
                "kind": "independently_isolated_visual_input_domain",
                "backendId": MACOS_NATIVE_BACKEND_ID,
                "surfaceId": format!("surface-{}", Uuid::new_v4()),
                "incarnation": format!("incarnation-{}", Uuid::new_v4()),
                "inputDomainId": format!("input-domain-{}", Uuid::new_v4()),
                "origin": "simulator_fixture",
                "observe": true,
                "semanticActions": true,
                "textEntry": true,
                "keyChords": true,
                "pointerFallback": true
            }))
            .unwrap();
        assert_eq!(native_simulator_origin, ComputerCapabilityProof::Unproven);
    }

    #[test]
    fn native_macos_cannot_be_stamped_isolated_or_background() {
        let surface = ComputerSurfaceBinding::issue();
        let isolated = ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
            backend_id: MACOS_NATIVE_BACKEND_ID.into(),
            surface_id: surface.surface_id.clone(),
            incarnation: surface.incarnation.clone(),
            input_domain_id: format!("input-domain-{}", Uuid::new_v4()),
            origin: IsolationProofOrigin::SimulatorFixture,
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: true,
            pointer_fallback: true,
        };
        assert_eq!(
            isolated.validate().unwrap_err().code,
            ComputerErrorCode::ForbiddenAction
        );
        let owner = Uuid::new_v4();
        assert_eq!(
            ComputerRun::new_with_isolation(
                owner,
                None,
                target(),
                Default::default(),
                ComputerPrincipal::local_operator(owner),
                surface.clone(),
                isolated,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );

        let background = ComputerCapabilityProof::MeasuredBackgroundSafeSemantic {
            backend_id: MACOS_NATIVE_BACKEND_ID.into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            measurement_id: format!("measurement-{}", Uuid::new_v4()),
        };
        let owner = Uuid::new_v4();
        assert_eq!(
            ComputerRun::new_with_isolation(
                owner,
                None,
                target(),
                Default::default(),
                ComputerPrincipal::local_operator(owner),
                surface,
                background,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );
    }

    #[test]
    fn public_agent_principal_minting_is_fail_closed_without_host_agent_registry() {
        let blocked = ComputerPrincipal::agent(format!("agent-{}", Uuid::new_v4()), 1).unwrap_err();
        assert_eq!(blocked.code, ComputerErrorCode::Unauthorized);
        assert_eq!(blocked.message, AGENT_PRINCIPAL_INTEGRATION_BLOCKER);
        assert!(ComputerPrincipal::agent("user-supplied", 1).is_err());
        let deserialized: ComputerPrincipal = serde_json::from_value(serde_json::json!({
            "kind": "agent",
            "agentId": format!("agent-{}", Uuid::new_v4()),
            "specRevision": 1
        }))
        .unwrap();
        assert!(deserialized.validate().is_ok());
        // Persisted Agent principals must validate so Runs can reopen. The
        // authority boundary is the private ComputerAuthorityToken
        // constructor plus AgentHost's durable Agent/spec lookup.
        let snake_legacy: ComputerPrincipal = serde_json::from_value(serde_json::json!({
            "kind": "agent",
            "agent_id": "agent-legacy",
            "spec_revision": 1
        }))
        .unwrap();
        assert!(snake_legacy.validate().is_ok());
        assert!(ComputerPrincipal::local_operator(Uuid::nil())
            .validate()
            .is_err());
    }

    #[test]
    fn isolated_proof_must_bind_the_exact_run_surface() {
        let owner = Uuid::new_v4();
        let surface = ComputerSurfaceBinding::issue();
        let other = ComputerSurfaceBinding::issue();
        let proof = ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
            backend_id: SIMULATOR_ISOLATED_BACKEND_ID.into(),
            surface_id: other.surface_id.clone(),
            incarnation: other.incarnation.clone(),
            input_domain_id: Uuid::new_v4().to_string(),
            origin: IsolationProofOrigin::SimulatorFixture,
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: true,
            pointer_fallback: true,
        };
        assert_eq!(
            ComputerRun::new_with_isolation(
                owner,
                None,
                target(),
                Default::default(),
                ComputerPrincipal::local_operator(owner),
                surface,
                proof,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenTarget
        );
    }

    #[test]
    fn macos_native_capability_helper_is_foreground_semantic_only() {
        let proof = macos_native_capability_proof();
        assert_eq!(proof.tier(), ComputerCapabilityTier::ForegroundSemantic);
        assert!(proof.observe());
        assert!(proof.semantic_actions());
        assert!(proof.text_entry());
        assert!(!proof.key_chords());
        assert!(!proof.pointer_fallback());
        assert!(!proof.isolated_input_is_dispatchable());
        assert_eq!(proof.backend_id(), MACOS_NATIVE_BACKEND_ID);
        let caps = ComputerCapabilities::from_proof(proof).unwrap();
        assert_eq!(caps.tier, ComputerCapabilityTier::ForegroundSemantic);
        assert!(!caps.pointer_fallback);
        assert!(!caps.key_chords);
        assert!(!caps.allows_action(&ComputerAction::PointerClick {
            x: 1.0,
            y: 1.0,
            button: PointerButton::Primary,
        }));
        assert!(caps.allows_action(&ComputerAction::ActivateTarget));
        let domain = macos_native_physical_input_domain();
        assert_eq!(domain, macos_native_physical_input_domain());
        assert_eq!(FOREGROUND_CONFLICT_DOMAIN_CAPACITY, 1);
    }

    #[test]
    fn public_new_is_unproven_without_self_issued_authority() {
        let run = ComputerRun::new(Uuid::new_v4(), None, target(), Default::default()).unwrap();
        assert_eq!(run.capability_proof, ComputerCapabilityProof::Unproven);
        assert!(run.initiating_principal.is_none());
        assert!(!run.surface.is_issued());
        assert_eq!(
            run.required_principal().unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }
}
