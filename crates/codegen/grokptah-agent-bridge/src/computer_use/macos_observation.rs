use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::platform::{
    ComputerBackgroundSafetyReceipt, ComputerObservationPlatform, ComputerPermission,
    ComputerPermissionStatus, ComputerPlatformStatus, ComputerTargetCandidate,
};
use super::types::{
    macos_background_safe_capability_proof, macos_native_capability_proof,
    macos_native_physical_input_domain, ActionOutcome, ComputerAction, ComputerBackend,
    ComputerBackendAttestation, ComputerCapabilities, ComputerCapabilityTier, ComputerError,
    ComputerErrorCode, ComputerObservation, ComputerResult, ComputerTarget, ComputerUseLimits,
    EvidenceRef, ObservationGeometry, PhysicalInputDomain, SemanticAction, SemanticElement,
    Sensitivity, MAX_LABEL_BYTES,
};
use super::{ComputerStore, ComputerUseService};

const MAX_TARGET_CANDIDATES: usize = 128;
const SELECTION_LEASE_TTL: StdDuration = StdDuration::from_secs(120);
const BACKGROUND_MEASUREMENT_TTL: StdDuration = StdDuration::from_secs(120);
const MAX_EVIDENCE_VAULT_BYTES: usize = 64 * 1024 * 1024;
const MIN_CAPTURE_INTERVAL: StdDuration = StdDuration::from_millis(500);
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

const HARD_DENIED_BUNDLE_IDS: &[&str] = &[
    "com.apple.securityagent",
    "com.apple.systemsettings",
    "com.apple.authorizationhost",
    "com.apple.controlcenter",
    "com.apple.keychainaccess",
    "com.apple.loginwindow",
    "com.apple.notificationcenterui",
    "com.apple.systempreferences",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "com.chriscase.grokptah",
    "com.dashlane.dashlane",
    "com.lastpass.lastpass",
    "org.keepassxc.keepassxc",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MacNativeIdentity {
    pub window_id: u32,
    pub process_id: i32,
    pub bundle_id: String,
}

impl MacNativeIdentity {
    pub(super) fn validate(&self) -> ComputerResult<()> {
        if self.window_id == 0
            || self.process_id <= 0
            || self.bundle_id.is_empty()
            || self.bundle_id.len() > 256
            || self.bundle_id.contains('\0')
        {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "macOS returned an invalid target identity",
            ));
        }
        Ok(())
    }

    /// Window/process/bundle identify the target, not a private input domain.
    #[allow(clippy::unused_self)]
    fn physical_input_domain(&self) -> PhysicalInputDomain {
        macos_native_physical_input_domain()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RawMacTarget {
    pub identity: MacNativeIdentity,
    pub application_name: String,
    pub frame: ObservationGeometry,
    pub on_screen: bool,
    pub active: bool,
    pub minimized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawMacSemanticAction {
    Invoke,
    SetValue,
    Select,
    Scroll,
}

#[derive(Debug, Clone)]
pub(crate) struct RawMacSemanticNode {
    pub role: String,
    pub subrole: Option<String>,
    pub label: Option<String>,
    pub value: Option<String>,
    /// Global logical screen coordinates from Accessibility.
    pub frame: Option<ObservationGeometry>,
    pub enabled: bool,
    pub focused: bool,
    pub sensitivity: Sensitivity,
    pub actions: Vec<RawMacSemanticAction>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawMacObservation {
    pub identity: MacNativeIdentity,
    pub captured_at: DateTime<Utc>,
    pub frame: ObservationGeometry,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub privacy_redacted: bool,
    pub screenshot_png: Vec<u8>,
    pub nodes: Vec<RawMacSemanticNode>,
    pub nodes_truncated: bool,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct RawMacActionRequest {
    pub target_frame: ObservationGeometry,
    pub element_index: Option<usize>,
    pub expected_element: Option<RawMacSemanticNode>,
    pub action: ComputerAction,
    pub execution_mode: MacSemanticExecutionMode,
    pub cancellation: MacActionCancellation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MacSemanticExecutionMode {
    #[default]
    ForegroundRequired,
    MeasuredBackground,
}

pub(crate) trait MacActionCancellationSignal: Send + Sync + std::fmt::Debug {
    fn cancel(&self);
    fn is_cancelled(&self) -> bool;

    /// Opaque pointer used only by the native shim while this signal's `Arc`
    /// is held across the blocking FFI call. Non-native fixtures return null.
    fn native_context(&self) -> *mut c_void {
        std::ptr::null_mut()
    }
}

#[derive(Debug, Default)]
struct LocalMacActionCancellation {
    cancelled: AtomicBool,
}

impl MacActionCancellationSignal for LocalMacActionCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MacActionCancellation {
    signal: Arc<dyn MacActionCancellationSignal>,
}

impl Default for MacActionCancellation {
    fn default() -> Self {
        Self::new(Arc::new(LocalMacActionCancellation::default()))
    }
}

impl MacActionCancellation {
    pub(crate) fn new(signal: Arc<dyn MacActionCancellationSignal>) -> Self {
        Self { signal }
    }

    pub(crate) fn cancel(&self) {
        self.signal.cancel();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }

    pub(crate) fn native_context(&self) -> *mut c_void {
        self.signal.native_context()
    }

    fn same_signal(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.signal, &other.signal)
    }
}

#[async_trait]
pub(crate) trait MacObservationSource: Send + Sync + std::fmt::Debug {
    fn status(&self) -> ComputerPlatformStatus;

    async fn request_permission(
        &self,
        permission: ComputerPermission,
    ) -> ComputerResult<ComputerPermissionStatus>;

    async fn list_targets(&self) -> ComputerResult<Vec<RawMacTarget>>;

    async fn revalidate_target(&self, identity: &MacNativeIdentity)
        -> ComputerResult<RawMacTarget>;

    async fn observe(
        &self,
        identity: &MacNativeIdentity,
        limits: &ComputerUseLimits,
    ) -> ComputerResult<RawMacObservation>;

    fn action_cancellation(&self) -> ComputerResult<MacActionCancellation> {
        Ok(MacActionCancellation::default())
    }

    /// Only the compiled-in native source and deterministic in-crate fixtures
    /// may claim that `MeasuredBackground` performs before/after host-state
    /// measurement. Third-party sources remain fail-closed.
    fn supports_measured_background(&self) -> bool {
        false
    }

    async fn act(
        &self,
        identity: &MacNativeIdentity,
        request: &RawMacActionRequest,
    ) -> ComputerResult<ActionOutcome>;
}

#[derive(Debug, Clone)]
struct TargetLease {
    issued_at: Instant,
    candidate: ComputerTargetCandidate,
    native: RawMacTarget,
}

#[derive(Debug, Clone)]
struct BackgroundMeasurementLease {
    issued_at: Instant,
    selection_token: String,
    target: ComputerTarget,
    native_identity: MacNativeIdentity,
    element_digest: String,
}

#[derive(Debug)]
pub struct MacOsObservationPlatform {
    source: Arc<dyn MacObservationSource>,
    leases: Mutex<HashMap<String, TargetLease>>,
    background_measurements: Mutex<HashMap<String, BackgroundMeasurementLease>>,
}

impl MacOsObservationPlatform {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn with_source(source: Arc<dyn MacObservationSource>) -> Self {
        Self {
            source,
            leases: Mutex::new(HashMap::new()),
            background_measurements: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn new_native() -> ComputerResult<Self> {
        Ok(Self::with_source(Arc::new(
            super::macos_native::NativeMacObservationSource::new()?,
        )))
    }

    fn target_lease(&self, selection_token: &str) -> ComputerResult<TargetLease> {
        super::types::validate_id("selection_token", selection_token)?;
        let lease = self
            .leases
            .lock()
            .get(selection_token)
            .cloned()
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "unknown or already-consumed computer-use selection",
                )
            })?;
        if lease.issued_at.elapsed() >= SELECTION_LEASE_TTL {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use selection expired",
            ));
        }
        Ok(lease)
    }

    async fn revalidate_lease(&self, lease: &TargetLease) -> ComputerResult<RawMacTarget> {
        require_platform_ready(&self.status())?;
        let current = self
            .source
            .revalidate_target(&lease.native.identity)
            .await?;
        validate_raw_target(&current)?;
        if current.identity != lease.native.identity || current.frame != lease.native.frame {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "selected macOS target identity or geometry changed",
            ));
        }
        Ok(current)
    }

    fn backend_from_lease(
        &self,
        lease: TargetLease,
        execution_mode: MacSemanticExecutionMode,
        measured_element_digest: Option<String>,
    ) -> Arc<dyn ComputerBackend> {
        Arc::new(MacOsObservationBackend {
            source: self.source.clone(),
            target: lease.candidate.target,
            native_identity: lease.native.identity,
            execution_mode,
            measured_element_digest,
            sequence: Mutex::new(0),
            observation_gate: tokio::sync::Mutex::new(()),
            last_capture_started: Mutex::new(None),
            cancellation_epochs: Mutex::new(HashMap::new()),
            action_gate: tokio::sync::Mutex::new(()),
            active_action_cancellation: Mutex::new(None),
            action_snapshot: Mutex::new(None),
            evidence: EvidenceVault::default(),
        })
    }

    async fn restore_background_probe(
        &self,
        lease: &TargetLease,
        element_index: usize,
        original_element: &RawMacSemanticNode,
        element_digest: &str,
        probe_text: &str,
        original_value: &str,
    ) -> ComputerResult<()> {
        let observed = self
            .source
            .observe(&lease.native.identity, &ComputerUseLimits::default())
            .await;
        let (restore_index, restore_element) = match observed {
            Ok(observation) => {
                if observation.identity != lease.native.identity
                    || observation.frame != lease.native.frame
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::UncertainOutcome,
                        "background probe target changed before disposable-value restoration",
                    ));
                }
                let (index, element) = observation
                    .nodes
                    .iter()
                    .enumerate()
                    .find(|(_, node)| mac_background_element_digest(node) == element_digest)
                    .ok_or_else(|| {
                        ComputerError::new(
                            ComputerErrorCode::UncertainOutcome,
                            "background probe element disappeared before restoration",
                        )
                    })?;
                if element.value.as_deref() == Some(original_value) {
                    return Ok(());
                }
                if element.value.as_deref() != Some(probe_text) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::UncertainOutcome,
                        "background probe element has an unexpected value before restoration",
                    ));
                }
                (index, element.clone())
            }
            Err(_) => {
                // The mutation may already have crossed the native boundary.
                // A synthesized exact expected value lets the shim restore it
                // only if the same semantic element currently contains the
                // probe value; otherwise native attestation fails closed.
                let mut expected = original_element.clone();
                expected.value = Some(probe_text.to_string());
                (element_index, expected)
            }
        };
        let restore = RawMacActionRequest {
            target_frame: lease.native.frame,
            element_index: Some(restore_index),
            expected_element: Some(restore_element),
            action: ComputerAction::SetValue {
                element_id: "background-probe-element".into(),
                text: original_value.to_string(),
            },
            execution_mode: MacSemanticExecutionMode::MeasuredBackground,
            cancellation: self.source.action_cancellation()?,
        };
        let outcome = self.source.act(&lease.native.identity, &restore).await?;
        if outcome.expected_postcondition_met != Some(true) {
            return Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "background probe could not prove restoration of the disposable value",
            ));
        }
        let restored = self
            .source
            .observe(&lease.native.identity, &ComputerUseLimits::default())
            .await?;
        if restored.identity != lease.native.identity
            || restored.frame != lease.native.frame
            || !restored.nodes.iter().any(|node| {
                mac_background_element_digest(node) == element_digest
                    && node.value.as_deref() == Some(original_value)
            })
        {
            return Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "background probe restoration did not survive exact-target re-observation",
            ));
        }
        Ok(())
    }

    async fn bind_target_backend(
        &self,
        selection_token: &str,
    ) -> ComputerResult<Arc<dyn ComputerBackend>> {
        let lease = self.target_lease(selection_token)?;
        self.revalidate_lease(&lease).await?;
        self.leases.lock().remove(selection_token);
        Ok(self.backend_from_lease(lease, MacSemanticExecutionMode::ForegroundRequired, None))
    }

    /// Calibrate one exact visible text-entry element on a disposable target.
    /// The probe performs a reversible value change through the native
    /// background path. Success means both mutations verified their direct AX
    /// postcondition while the native shim measured no foreground app, active
    /// window, or physical-pointer change. The returned proof is short-lived,
    /// one-use, and process/window/element bound.
    pub async fn measure_background_text_entry(
        &self,
        selection_token: &str,
        element_label: &str,
        probe_text: &str,
        disposable_target_acknowledged: bool,
    ) -> ComputerResult<ComputerBackgroundSafetyReceipt> {
        if !disposable_target_acknowledged {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "background calibration requires explicit disposable-target acknowledgement",
            ));
        }
        if !self.source.supports_measured_background() {
            return Err(ComputerError::new(
                ComputerErrorCode::UnsupportedPlatform,
                "this macOS source cannot measure background dispatch",
            ));
        }
        let label = bounded_required(element_label, MAX_LABEL_BYTES).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "background probe element label is missing or too long",
            )
        })?;
        let probe_action = ComputerAction::SetValue {
            element_id: "background-probe-element".into(),
            text: probe_text.to_string(),
        };
        probe_action.validate(&ComputerUseLimits::default())?;

        let lease = self.target_lease(selection_token)?;
        let current = self.revalidate_lease(&lease).await?;
        if current.active || !current.on_screen || current.minimized {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "background calibration requires a visible, non-minimized target that is not foreground",
            ));
        }
        let before = self
            .source
            .observe(&lease.native.identity, &ComputerUseLimits::default())
            .await?;
        if before.identity != lease.native.identity
            || before.frame != lease.native.frame
            || before.nodes_truncated
            || before.sensitivity.is_hard_denied()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "background calibration target is stale, truncated, or sensitive",
            ));
        }
        let mut matches = before.nodes.iter().enumerate().filter(|(_, node)| {
            node.label.as_deref() == Some(label.as_str())
                && node.enabled
                && !mac_node_is_secure(node)
                && !node.sensitivity.is_hard_denied()
                && node.actions.contains(&RawMacSemanticAction::SetValue)
        });
        let (element_index, element) = matches.next().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "no exact visible non-sensitive text-entry element matched the probe label",
            )
        })?;
        if matches.next().is_some() {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "background probe element label is ambiguous",
            ));
        }
        let original_value = element.value.clone().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "background text-entry calibration requires a readable reversible value",
            )
        })?;
        ComputerAction::SetValue {
            element_id: "background-probe-element".into(),
            text: original_value.clone(),
        }
        .validate(&ComputerUseLimits::default())?;
        if original_value == probe_text {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "background probe value must differ from the current disposable value",
            ));
        }
        let element_digest = mac_background_element_digest(element);
        let first = RawMacActionRequest {
            target_frame: before.frame,
            element_index: Some(element_index),
            expected_element: Some(element.clone()),
            action: probe_action,
            execution_mode: MacSemanticExecutionMode::MeasuredBackground,
            cancellation: self.source.action_cancellation()?,
        };
        let first_outcome = self.source.act(&lease.native.identity, &first).await;
        let first_proved = first_outcome
            .as_ref()
            .is_ok_and(|outcome| outcome.expected_postcondition_met == Some(true));
        let restoration = self
            .restore_background_probe(
                &lease,
                element_index,
                element,
                &element_digest,
                probe_text,
                &original_value,
            )
            .await;
        if !first_proved || restoration.is_err() {
            return Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "background calibration did not prove both mutation and disposable-value restoration",
            ));
        }
        let final_target = self.revalidate_lease(&lease).await?;
        if final_target.active || !final_target.on_screen || final_target.minimized {
            return Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "background probe changed target visibility or foreground state",
            ));
        }

        let measurement_token = Uuid::new_v4().to_string();
        self.background_measurements.lock().insert(
            measurement_token.clone(),
            BackgroundMeasurementLease {
                issued_at: Instant::now(),
                selection_token: selection_token.to_string(),
                target: lease.candidate.target.clone(),
                native_identity: lease.native.identity,
                element_digest,
            },
        );
        Ok(ComputerBackgroundSafetyReceipt {
            measurement_token,
            target: lease.candidate.target,
            supported_action_classes: BTreeSet::from([super::types::ActionClass::TextEntry]),
            valid_for_millis: BACKGROUND_MEASUREMENT_TTL.as_millis() as u64,
        })
    }

    pub async fn bind_measured_background_target_service(
        &self,
        selection_token: &str,
        measurement_token: &str,
        store: ComputerStore,
    ) -> ComputerResult<ComputerUseService> {
        super::types::validate_id("measurement_token", measurement_token)?;
        let measurement = self
            .background_measurements
            .lock()
            .remove(measurement_token)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "unknown or already-consumed background measurement",
                )
            })?;
        if measurement.issued_at.elapsed() >= BACKGROUND_MEASUREMENT_TTL
            || measurement.selection_token != selection_token
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "background measurement expired or does not bind this selection",
            ));
        }
        let lease = self.target_lease(selection_token)?;
        if lease.candidate.target != measurement.target
            || lease.native.identity != measurement.native_identity
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "background measurement does not bind the selected target generation",
            ));
        }
        let current = self.revalidate_lease(&lease).await?;
        if current.active || !current.on_screen || current.minimized {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "measured background target became foreground, hidden, or minimized",
            ));
        }
        self.leases.lock().remove(selection_token);
        let backend = self.backend_from_lease(
            lease,
            MacSemanticExecutionMode::MeasuredBackground,
            Some(measurement.element_digest),
        );
        let attestation = ComputerBackendAttestation::trusted(
            super::types::MACOS_BACKGROUND_SAFE_BACKEND_ID,
            ComputerCapabilityTier::MeasuredBackgroundSafeSemantic,
            macos_native_physical_input_domain(),
        )?;
        Ok(ComputerUseService::new_trusted(backend, store, attestation))
    }

    #[cfg(test)]
    async fn bind_target(&self, selection_token: &str) -> ComputerResult<Arc<dyn ComputerBackend>> {
        self.bind_target_backend(selection_token).await
    }
}

#[async_trait]
impl ComputerObservationPlatform for MacOsObservationPlatform {
    fn status(&self) -> ComputerPlatformStatus {
        let status = self.source.status();
        if status.validate().is_ok() {
            status
        } else {
            ComputerPlatformStatus {
                platform_id: "macos".into(),
                available: false,
                minimum_os_version: Some("14.0".into()),
                screen_recording: ComputerPermissionStatus::Restricted,
                accessibility: ComputerPermissionStatus::Restricted,
                detail: Some("native computer-use status was invalid".into()),
            }
        }
    }

    async fn request_permission(
        &self,
        permission: ComputerPermission,
    ) -> ComputerResult<ComputerPermissionStatus> {
        self.source.request_permission(permission).await
    }

    async fn list_targets(&self) -> ComputerResult<Vec<ComputerTargetCandidate>> {
        // Even a failed refresh invalidates the prior picker snapshot. Keeping
        // an old token after permissions or native enumeration fail would make
        // the visible picker state weaker than the authorization state.
        self.leases.lock().clear();
        self.background_measurements.lock().clear();
        require_platform_ready(&self.status())?;
        let raw_targets = self.source.list_targets().await?;
        let mut candidates = Vec::new();
        let mut leases = self.leases.lock();

        for raw in raw_targets.into_iter().take(MAX_TARGET_CANDIDATES * 2) {
            if candidates.len() == MAX_TARGET_CANDIDATES
                || validate_raw_target(&raw).is_err()
                || hard_denied_bundle(&raw.identity.bundle_id)
            {
                continue;
            }
            let selection_token = Uuid::new_v4().to_string();
            let generation_bytes = *Uuid::new_v4().as_bytes();
            let generation = u64::from_be_bytes(
                generation_bytes[..8]
                    .try_into()
                    .expect("UUID prefix has eight bytes"),
            )
            .max(1);
            let candidate = ComputerTargetCandidate {
                selection_token: selection_token.clone(),
                target: ComputerTarget {
                    app_id: raw.identity.bundle_id.clone(),
                    window_id: format!("window-{}", Uuid::new_v4()),
                    generation,
                    display_name: bounded_required(&raw.application_name, MAX_LABEL_BYTES)
                        .unwrap_or_else(|| "Application".into()),
                    sensitivity: Sensitivity::None,
                },
                geometry: raw.frame,
                on_screen: raw.on_screen,
                active: raw.active,
                minimized: raw.minimized,
            };
            candidate.validate()?;
            leases.insert(
                selection_token,
                TargetLease {
                    issued_at: Instant::now(),
                    candidate: candidate.clone(),
                    native: raw,
                },
            );
            candidates.push(candidate);
        }
        Ok(candidates)
    }

    async fn bind_target_service(
        &self,
        selection_token: &str,
        store: ComputerStore,
    ) -> ComputerResult<ComputerUseService> {
        let backend = self.bind_target_backend(selection_token).await?;
        let attestation = ComputerBackendAttestation::trusted(
            super::types::MACOS_NATIVE_BACKEND_ID,
            ComputerCapabilityTier::ForegroundSemantic,
            macos_native_physical_input_domain(),
        )?;
        Ok(ComputerUseService::new_trusted(backend, store, attestation))
    }

    async fn measure_background_text_entry(
        &self,
        selection_token: &str,
        element_label: &str,
        probe_text: &str,
        disposable_target_acknowledged: bool,
    ) -> ComputerResult<ComputerBackgroundSafetyReceipt> {
        MacOsObservationPlatform::measure_background_text_entry(
            self,
            selection_token,
            element_label,
            probe_text,
            disposable_target_acknowledged,
        )
        .await
    }

    async fn bind_measured_background_target_service(
        &self,
        selection_token: &str,
        measurement_token: &str,
        store: ComputerStore,
    ) -> ComputerResult<ComputerUseService> {
        MacOsObservationPlatform::bind_measured_background_target_service(
            self,
            selection_token,
            measurement_token,
            store,
        )
        .await
    }
}

#[derive(Debug, Clone)]
struct MacActionSnapshot {
    run_id: String,
    observation_id: String,
    target_frame: ObservationGeometry,
    nodes: Vec<RawMacSemanticNode>,
    nodes_truncated: bool,
    identity: MacNativeIdentity,
    content_digest: String,
    shape_digest: String,
}

#[derive(Debug)]
struct MacOsObservationBackend {
    source: Arc<dyn MacObservationSource>,
    target: ComputerTarget,
    native_identity: MacNativeIdentity,
    execution_mode: MacSemanticExecutionMode,
    measured_element_digest: Option<String>,
    sequence: Mutex<u64>,
    observation_gate: tokio::sync::Mutex<()>,
    last_capture_started: Mutex<Option<Instant>>,
    cancellation_epochs: Mutex<HashMap<String, u64>>,
    action_gate: tokio::sync::Mutex<()>,
    active_action_cancellation: Mutex<Option<(String, MacActionCancellation)>>,
    action_snapshot: Mutex<Option<MacActionSnapshot>>,
    evidence: EvidenceVault,
}

#[async_trait]
impl ComputerBackend for MacOsObservationBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        let proof = match self.execution_mode {
            MacSemanticExecutionMode::ForegroundRequired => macos_native_capability_proof(),
            MacSemanticExecutionMode::MeasuredBackground => {
                macos_background_safe_capability_proof()
            }
        };
        ComputerCapabilities::from_proof(proof)
            .expect("compiled-in macOS native capability proof is valid")
    }

    fn physical_input_domain(&self) -> PhysicalInputDomain {
        self.native_identity.physical_input_domain()
    }

    async fn observe(
        &self,
        run_id: &str,
        observation_id: &str,
        target: &ComputerTarget,
        limits: &ComputerUseLimits,
    ) -> ComputerResult<ComputerObservation> {
        if target != &self.target {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "macOS backend is bound to a different local selection",
            ));
        }
        let _capture_guard = self.observation_gate.lock().await;
        let wait = self
            .last_capture_started
            .lock()
            .and_then(|started| MIN_CAPTURE_INTERVAL.checked_sub(started.elapsed()));
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        *self.last_capture_started.lock() = Some(Instant::now());
        let epoch = self.cancellation_epoch(run_id);
        let raw = self.source.observe(&self.native_identity, limits).await?;
        if self.cancellation_epoch(run_id) != epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "macOS observation was cancelled",
            ));
        }
        if raw.identity != self.native_identity {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "macOS observation target identity changed",
            ));
        }
        let action_snapshot = MacActionSnapshot {
            run_id: run_id.to_string(),
            observation_id: String::new(),
            target_frame: raw.frame,
            nodes: raw.nodes.clone(),
            nodes_truncated: raw.nodes_truncated,
            identity: raw.identity.clone(),
            content_digest: mac_content_digest(&raw.nodes),
            shape_digest: mac_shape_digest(&raw.nodes),
        };
        let observation = normalize_observation(
            run_id,
            observation_id,
            &self.target,
            raw,
            limits,
            &self.sequence,
            &self.evidence,
        )?;
        if self.cancellation_epoch(run_id) != epoch {
            self.evidence.remove_run(run_id);
            return Err(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "macOS observation was cancelled",
            ));
        }
        *self.action_snapshot.lock() = Some(MacActionSnapshot {
            observation_id: observation.observation_id.clone(),
            ..action_snapshot
        });
        Ok(observation)
    }

    async fn act(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        self.act_if_current(run_id, observation, action).await
    }

    async fn act_if_current(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        if observation.target != self.target || observation.sensitivity.is_hard_denied() {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "macOS action is bound to a different or sensitive target",
            ));
        }
        if observation.elements_truncated {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "macOS semantic tree was truncated; action dispatch is denied",
            ));
        }
        let _action_guard = self.action_gate.lock().await;
        let epoch = self.cancellation_epoch(run_id);
        let snapshot = self
            .action_snapshot
            .lock()
            .clone()
            .filter(|snapshot| {
                snapshot.run_id == run_id && snapshot.observation_id == observation.observation_id
            })
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "macOS action observation is stale",
                )
            })?;
        if let Err(error) = require_platform_ready(&self.source.status()) {
            *self.action_snapshot.lock() = None;
            return Err(error);
        }
        if snapshot.nodes_truncated {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "macOS native semantic walk was truncated; action dispatch is denied",
            ));
        }
        let live = self
            .source
            .observe(&self.native_identity, &ComputerUseLimits::default())
            .await?;
        if live.identity != snapshot.identity
            || live.identity != self.native_identity
            || live.frame != snapshot.target_frame
            || mac_shape_digest(&live.nodes) != snapshot.shape_digest
        {
            *self.action_snapshot.lock() = None;
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "macOS action target geometry or selected-element shape changed",
            ));
        }
        if mac_content_digest(&live.nodes) != snapshot.content_digest
            || live.nodes_truncated != snapshot.nodes_truncated
        {
            *self.action_snapshot.lock() = None;
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "macOS action is not attested against the current AX/tree generation",
            ));
        }
        let (element_index, expected_element) =
            action_element(snapshot.nodes.as_slice(), observation, action)?;
        if self.execution_mode == MacSemanticExecutionMode::MeasuredBackground {
            let current = self.source.revalidate_target(&self.native_identity).await?;
            validate_raw_target(&current)?;
            if current.identity != self.native_identity
                || current.frame != snapshot.target_frame
                || current.active
                || !current.on_screen
                || current.minimized
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "measured background dispatch requires the exact visible target to remain in the background",
                ));
            }
            if !matches!(action, ComputerAction::SetValue { .. })
                || expected_element.as_ref().is_none_or(|element| {
                    self.measured_element_digest.as_deref()
                        != Some(mac_background_element_digest(element).as_str())
                })
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "this measurement authorizes text entry on one exact semantic element only",
                ));
            }
        }
        let cancellation = self.source.action_cancellation()?;
        let request = RawMacActionRequest {
            target_frame: snapshot.target_frame,
            element_index,
            expected_element,
            action: action.clone(),
            execution_mode: self.execution_mode,
            cancellation: cancellation.clone(),
        };
        *self.active_action_cancellation.lock() = Some((run_id.to_string(), cancellation.clone()));
        if self.cancellation_epoch(run_id) != epoch {
            cancellation.cancel();
            self.clear_action_cancellation(run_id, &cancellation);
            return Err(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "macOS action was cancelled before dispatch",
            ));
        }
        *self.action_snapshot.lock() = None;
        let outcome = self.source.act(&self.native_identity, &request).await;
        self.clear_action_cancellation(run_id, &cancellation);
        if cancellation.is_cancelled() || self.cancellation_epoch(run_id) != epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "macOS action completion lost to local takeover",
            ));
        }
        let outcome = outcome?;
        if outcome.expected_postcondition_met == Some(false) {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "macOS action postcondition was not verified",
            ));
        }
        Ok(ActionOutcome::bounded(
            outcome.summary,
            outcome.expected_postcondition_met,
        ))
    }

    async fn read_evidence(&self, run_id: &str, asset_id: &str) -> ComputerResult<Option<Vec<u8>>> {
        Ok(self.evidence.read(run_id, asset_id))
    }

    async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
        self.bump_cancellation_epoch(run_id);
        if let Some((_, cancellation)) = self
            .active_action_cancellation
            .lock()
            .as_ref()
            .filter(|(active_run_id, _)| active_run_id == run_id)
        {
            cancellation.cancel();
        }
        let mut snapshot = self.action_snapshot.lock();
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.run_id == run_id)
        {
            *snapshot = None;
        }
        self.evidence.remove_run(run_id);
        Ok(())
    }
}

impl MacOsObservationBackend {
    fn cancellation_epoch(&self, run_id: &str) -> u64 {
        *self
            .cancellation_epochs
            .lock()
            .entry(run_id.to_string())
            .or_default()
    }

    fn bump_cancellation_epoch(&self, run_id: &str) {
        let mut epochs = self.cancellation_epochs.lock();
        let epoch = epochs.entry(run_id.to_string()).or_default();
        *epoch = epoch.saturating_add(1);
    }

    fn clear_action_cancellation(&self, run_id: &str, completed: &MacActionCancellation) {
        let mut active = self.active_action_cancellation.lock();
        if active.as_ref().is_some_and(|(active_run_id, current)| {
            active_run_id == run_id && current.same_signal(completed)
        }) {
            *active = None;
        }
    }
}

fn mac_content_digest(nodes: &[RawMacSemanticNode]) -> String {
    let mut hasher = Sha256::new();
    for node in nodes {
        hasher.update(node.label.as_deref().unwrap_or("").as_bytes());
        hasher.update([0]);
        hasher.update(node.value.as_deref().unwrap_or("").as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn mac_shape_digest(nodes: &[RawMacSemanticNode]) -> String {
    let mut hasher = Sha256::new();
    for node in nodes {
        hasher.update(node.role.as_bytes());
        hasher.update([0]);
        hasher.update(format!("{:?}", node.frame).as_bytes());
        hasher.update([0]);
        hasher.update([u8::from(node.enabled), u8::from(node.focused)]);
        hasher.update(format!("{:?}", node.actions).as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn mac_background_element_digest(node: &RawMacSemanticNode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(node.role.as_bytes());
    hasher.update([0]);
    hasher.update(node.subrole.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    hasher.update(node.label.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    hasher.update(format!("{:?}", node.frame).as_bytes());
    hasher.update([u8::from(node.enabled)]);
    hasher.update(format!("{:?}", node.sensitivity).as_bytes());
    hasher.update([0]);
    hasher.update(format!("{:?}", node.actions).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn mac_node_is_secure(node: &RawMacSemanticNode) -> bool {
    node.role.to_ascii_lowercase().contains("secure")
        || node
            .subrole
            .as_ref()
            .is_some_and(|subrole| subrole.to_ascii_lowercase().contains("secure"))
}

fn action_element(
    raw_nodes: &[RawMacSemanticNode],
    observation: &ComputerObservation,
    action: &ComputerAction,
) -> ComputerResult<(Option<usize>, Option<RawMacSemanticNode>)> {
    if matches!(action, ComputerAction::ActivateTarget) {
        return Ok((None, None));
    }
    let element_id = match action {
        ComputerAction::ActivateTarget => unreachable!("activation returned above"),
        ComputerAction::Invoke { element_id }
        | ComputerAction::SetValue { element_id, .. }
        | ComputerAction::Select { element_id } => element_id,
        ComputerAction::Scroll {
            element_id: Some(element_id),
            ..
        } => element_id,
        ComputerAction::Scroll {
            element_id: None, ..
        } => {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "macOS semantic scroll requires an observed element",
            ))
        }
        ComputerAction::KeyChord { .. }
        | ComputerAction::PointerClick { .. }
        | ComputerAction::Wait { .. } => {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "macOS native backend accepts semantic Accessibility actions only",
            ))
        }
    };
    let exposed = observation.element(element_id).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::Conflict,
            "macOS action element is absent from the current observation",
        )
    })?;
    if !exposed.enabled || exposed.sensitivity.is_hard_denied() {
        return Err(ComputerError::new(
            ComputerErrorCode::SensitiveSurface,
            "macOS action element is disabled or sensitive",
        ));
    }
    let prefix = format!("{}-element-", observation.observation_id);
    let element_index = element_id
        .strip_prefix(&prefix)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|index| *index < raw_nodes.len())
        .ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Conflict,
                "macOS action element identity is malformed or stale",
            )
        })?;
    let raw = raw_nodes[element_index].clone();
    let required = match action {
        ComputerAction::Invoke { .. } => RawMacSemanticAction::Invoke,
        ComputerAction::SetValue { .. } => RawMacSemanticAction::SetValue,
        ComputerAction::Select { .. } => RawMacSemanticAction::Select,
        ComputerAction::Scroll { .. } => RawMacSemanticAction::Scroll,
        _ => unreachable!("non-element actions returned above"),
    };
    if raw.sensitivity.is_hard_denied()
        || !raw.enabled
        || !raw.actions.contains(&required)
        || raw.role != exposed.role
        || raw.label != exposed.label
    {
        return Err(ComputerError::new(
            ComputerErrorCode::Conflict,
            "macOS raw element no longer matches the exposed observation",
        ));
    }
    Ok((Some(element_index), Some(raw)))
}

fn normalize_observation(
    run_id: &str,
    observation_id: &str,
    target: &ComputerTarget,
    raw: RawMacObservation,
    limits: &ComputerUseLimits,
    sequence: &Mutex<u64>,
    evidence: &EvidenceVault,
) -> ComputerResult<ComputerObservation> {
    raw.identity.validate()?;
    raw.frame.validate()?;
    if raw.sensitivity.is_hard_denied() {
        return Err(ComputerError::new(
            ComputerErrorCode::SensitiveSurface,
            "macOS observed a hard-denied surface",
        ));
    }
    if !raw.privacy_redacted {
        return Err(ComputerError::new(
            ComputerErrorCode::SensitiveSurface,
            "macOS screenshot did not pass privacy redaction",
        ));
    }
    if raw.pixel_width == 0
        || raw.pixel_height == 0
        || raw.pixel_width > limits.max_screenshot_dimension
        || raw.pixel_height > limits.max_screenshot_dimension
        || raw.screenshot_png.len() as u64 > limits.max_screenshot_bytes
        || !raw.screenshot_png.starts_with(PNG_SIGNATURE)
    {
        return Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "macOS returned invalid or oversized screenshot evidence",
        ));
    }
    if png_dimensions(&raw.screenshot_png) != Some((raw.pixel_width, raw.pixel_height)) {
        return Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "macOS screenshot dimensions do not match the PNG evidence",
        ));
    }

    let mut next_sequence = sequence.lock();
    *next_sequence = next_sequence.saturating_add(1);
    let current_sequence = *next_sequence;
    drop(next_sequence);
    let scale_factor =
        (raw.pixel_width as f64 / raw.frame.width).min(raw.pixel_height as f64 / raw.frame.height);
    let geometry = ObservationGeometry {
        scale_factor,
        ..raw.frame
    };
    geometry.validate()?;

    let mut elements = Vec::new();
    let mut semantic_bytes = 0_u64;
    let mut elements_truncated = raw.nodes_truncated;
    for (index, node) in raw.nodes.into_iter().enumerate() {
        if elements.len() >= limits.max_semantic_elements as usize {
            elements_truncated = true;
            break;
        }
        let Some(role) = bounded_required(&node.role, 128) else {
            elements_truncated = true;
            continue;
        };
        let sensitivity = if mac_node_is_secure(&node) {
            Sensitivity::Secure
        } else {
            node.sensitivity
        };
        // Native capture uses these nodes to redact pixels. They are omitted
        // from the exposed semantic tree so a model can neither read nor
        // target a secure/system control.
        if sensitivity.is_hard_denied() {
            continue;
        }
        let element = SemanticElement {
            element_id: format!("{}-element-{index}", observation_id),
            role,
            label: node
                .label
                .as_deref()
                .and_then(|value| bounded_required(value, MAX_LABEL_BYTES)),
            value: if sensitivity.is_hard_denied() {
                None
            } else {
                node.value
                    .as_deref()
                    .and_then(|value| bounded_required(value, MAX_LABEL_BYTES))
            },
            bounds: node
                .frame
                .and_then(|frame| target_relative_bounds(frame, raw.frame, scale_factor)),
            enabled: node.enabled,
            focused: node.focused,
            sensitivity,
            actions: node
                .actions
                .into_iter()
                .map(|action| match action {
                    RawMacSemanticAction::Invoke => SemanticAction::Invoke,
                    RawMacSemanticAction::SetValue => SemanticAction::SetValue,
                    RawMacSemanticAction::Select => SemanticAction::Select,
                    RawMacSemanticAction::Scroll => SemanticAction::Scroll,
                })
                .collect::<BTreeSet<_>>(),
        };
        element.validate()?;
        let encoded_len = serde_json::to_vec(&element)
            .map_err(|error| ComputerError::new(ComputerErrorCode::Internal, error.to_string()))?
            .len() as u64;
        if semantic_bytes.saturating_add(encoded_len) > limits.max_semantic_bytes {
            elements_truncated = true;
            break;
        }
        semantic_bytes = semantic_bytes.saturating_add(encoded_len);
        elements.push(element);
    }

    // A truncated Accessibility walk cannot prove that every secure field was
    // found. Keep the bounded semantic snapshot, but do not expose its image.
    let screenshot = if raw.nodes_truncated {
        evidence.remove_run(run_id);
        None
    } else {
        Some(evidence.insert(
            run_id,
            raw.screenshot_png,
            raw.pixel_width,
            raw.pixel_height,
            limits,
        )?)
    };
    let observation = ComputerObservation {
        observation_id: observation_id.to_string(),
        sequence: current_sequence,
        target: target.clone(),
        captured_at: raw.captured_at,
        geometry,
        screenshot,
        elements,
        elements_truncated,
        sensitivity: raw.sensitivity,
        authority: Default::default(),
    };
    if let Err(error) = observation.validate(limits) {
        evidence.remove_run(run_id);
        return Err(error);
    }
    Ok(observation)
}

fn target_relative_bounds(
    frame: ObservationGeometry,
    target: ObservationGeometry,
    scale_factor: f64,
) -> Option<ObservationGeometry> {
    if frame.validate().is_err() {
        return None;
    }
    let left = frame.x.max(target.x);
    let top = frame.y.max(target.y);
    let right = (frame.x + frame.width).min(target.x + target.width);
    let bottom = (frame.y + frame.height).min(target.y + target.height);
    if right <= left || bottom <= top {
        return None;
    }
    Some(ObservationGeometry {
        x: left - target.x,
        y: top - target.y,
        width: right - left,
        height: bottom - top,
        scale_factor,
    })
}

fn validate_raw_target(raw: &RawMacTarget) -> ComputerResult<()> {
    raw.identity.validate()?;
    raw.frame.validate()?;
    if bounded_required(&raw.application_name, MAX_LABEL_BYTES).is_none() {
        return Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "macOS returned an invalid application label",
        ));
    }
    Ok(())
}

fn hard_denied_bundle(bundle_id: &str) -> bool {
    HARD_DENIED_BUNDLE_IDS
        .iter()
        .any(|denied| bundle_id.eq_ignore_ascii_case(denied))
}

fn bounded_required(value: &str, max_bytes: usize) -> Option<String> {
    let sanitized = value.replace('\0', "");
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(crate::textutil::truncate_at_char_boundary(trimmed, max_bytes).to_string())
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(PNG_SIGNATURE) || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn require_platform_ready(status: &ComputerPlatformStatus) -> ComputerResult<()> {
    status.validate()?;
    if !status.available {
        return Err(ComputerError::new(
            ComputerErrorCode::UnsupportedPlatform,
            status
                .detail
                .clone()
                .unwrap_or_else(|| "macOS Computer Use is unavailable".into()),
        ));
    }
    require_permission(status.screen_recording, "Screen Recording")?;
    require_permission(status.accessibility, "Accessibility")?;
    Ok(())
}

fn require_permission(status: ComputerPermissionStatus, name: &str) -> ComputerResult<()> {
    let code = match status {
        ComputerPermissionStatus::Granted => return Ok(()),
        ComputerPermissionStatus::Missing | ComputerPermissionStatus::PromptPending => {
            ComputerErrorCode::PermissionRequired
        }
        ComputerPermissionStatus::Denied | ComputerPermissionStatus::Restricted => {
            ComputerErrorCode::PermissionDenied
        }
        ComputerPermissionStatus::Revoked => ComputerErrorCode::PermissionRevoked,
        ComputerPermissionStatus::Unsupported => ComputerErrorCode::UnsupportedPlatform,
    };
    Err(ComputerError::new(
        code,
        format!("macOS {name} permission is not granted"),
    ))
}

#[derive(Debug)]
struct StoredEvidence {
    asset_id: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct EvidenceVaultState {
    entries: HashMap<String, StoredEvidence>,
    order: VecDeque<String>,
    total_bytes: usize,
}

#[derive(Debug, Default)]
struct EvidenceVault {
    state: Mutex<EvidenceVaultState>,
}

impl EvidenceVault {
    fn insert(
        &self,
        run_id: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        limits: &ComputerUseLimits,
    ) -> ComputerResult<EvidenceRef> {
        if bytes.len() as u64 > limits.max_screenshot_bytes
            || bytes.len() > MAX_EVIDENCE_VAULT_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "macOS screenshot exceeds the evidence vault bound",
            ));
        }
        let mut state = self.state.lock();
        remove_evidence_entry(&mut state, run_id);
        while state.total_bytes.saturating_add(bytes.len()) > MAX_EVIDENCE_VAULT_BYTES {
            let Some(oldest_run) = state.order.pop_front() else {
                break;
            };
            if let Some(removed) = state.entries.remove(&oldest_run) {
                state.total_bytes = state.total_bytes.saturating_sub(removed.bytes.len());
            }
        }
        let asset_id = Uuid::new_v4().to_string();
        let content_sha256 = format!("{:x}", Sha256::digest(&bytes));
        state.total_bytes = state.total_bytes.saturating_add(bytes.len());
        state.order.push_back(run_id.into());
        state.entries.insert(
            run_id.into(),
            StoredEvidence {
                asset_id: asset_id.clone(),
                bytes,
            },
        );
        let byte_len = state
            .entries
            .get(run_id)
            .map_or(0, |stored| stored.bytes.len() as u64);
        Ok(EvidenceRef {
            content_sha256,
            media_type: "image/png".into(),
            byte_len,
            width,
            height,
            redacted: true,
            asset_id,
        })
    }

    fn read(&self, run_id: &str, asset_id: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .entries
            .get(run_id)
            .filter(|stored| stored.asset_id == asset_id)
            .map(|stored| stored.bytes.clone())
    }

    fn remove_run(&self, run_id: &str) {
        remove_evidence_entry(&mut self.state.lock(), run_id);
    }
}

fn remove_evidence_entry(state: &mut EvidenceVaultState, run_id: &str) {
    if let Some(removed) = state.entries.remove(run_id) {
        state.total_bytes = state.total_bytes.saturating_sub(removed.bytes.len());
    }
    state.order.retain(|entry| entry != run_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_PIXEL_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31,
        0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[derive(Debug)]
    struct FixtureSource {
        status: Mutex<ComputerPlatformStatus>,
        targets: Mutex<Vec<RawMacTarget>>,
        observation_identity: Mutex<Option<MacNativeIdentity>>,
        nodes_truncated: Mutex<bool>,
        observation_frame: Mutex<ObservationGeometry>,
        pixel_dimensions: Mutex<(u32, u32)>,
        list_error: Mutex<Option<ComputerErrorCode>>,
        observation_error: Mutex<Option<ComputerErrorCode>>,
        action_error: Mutex<Option<ComputerErrorCode>>,
        action_postcondition: Mutex<Option<bool>>,
        action_requests: Mutex<Vec<RawMacActionRequest>>,
        content_label: Mutex<String>,
        text_value: Mutex<String>,
        background_interference: AtomicBool,
        block_action_before_dispatch: AtomicBool,
        action_entered: tokio::sync::Notify,
        release_action: tokio::sync::Notify,
    }

    impl FixtureSource {
        fn granted() -> Self {
            Self {
                status: Mutex::new(ComputerPlatformStatus {
                    platform_id: "macos".into(),
                    available: true,
                    minimum_os_version: Some("14.0".into()),
                    screen_recording: ComputerPermissionStatus::Granted,
                    accessibility: ComputerPermissionStatus::Granted,
                    detail: None,
                }),
                targets: Mutex::new(vec![fixture_target("com.example.demo", 42, -1200.0)]),
                observation_identity: Mutex::new(None),
                nodes_truncated: Mutex::new(false),
                observation_frame: Mutex::new(ObservationGeometry {
                    x: -1200.0,
                    y: 40.0,
                    width: 4.0,
                    height: 4.0,
                    scale_factor: 1.0,
                }),
                pixel_dimensions: Mutex::new((1, 1)),
                list_error: Mutex::new(None),
                observation_error: Mutex::new(None),
                action_error: Mutex::new(None),
                action_postcondition: Mutex::new(None),
                action_requests: Mutex::new(Vec::new()),
                content_label: Mutex::new("Continue".into()),
                text_value: Mutex::new("public-demo-value".into()),
                background_interference: AtomicBool::new(false),
                block_action_before_dispatch: AtomicBool::new(false),
                action_entered: tokio::sync::Notify::new(),
                release_action: tokio::sync::Notify::new(),
            }
        }
    }

    #[async_trait]
    impl MacObservationSource for FixtureSource {
        fn status(&self) -> ComputerPlatformStatus {
            self.status.lock().clone()
        }

        async fn request_permission(
            &self,
            permission: ComputerPermission,
        ) -> ComputerResult<ComputerPermissionStatus> {
            let mut status = self.status.lock();
            match permission {
                ComputerPermission::ScreenRecording => {
                    status.screen_recording = ComputerPermissionStatus::Granted
                }
                ComputerPermission::Accessibility => {
                    status.accessibility = ComputerPermissionStatus::Granted
                }
            }
            Ok(ComputerPermissionStatus::Granted)
        }

        async fn list_targets(&self) -> ComputerResult<Vec<RawMacTarget>> {
            if let Some(code) = *self.list_error.lock() {
                return Err(ComputerError::new(code, "fixture target listing failed"));
            }
            Ok(self.targets.lock().clone())
        }

        async fn revalidate_target(
            &self,
            identity: &MacNativeIdentity,
        ) -> ComputerResult<RawMacTarget> {
            self.targets
                .lock()
                .iter()
                .find(|target| &target.identity == identity)
                .cloned()
                .ok_or_else(|| ComputerError::new(ComputerErrorCode::TargetClosed, "target closed"))
        }

        async fn observe(
            &self,
            identity: &MacNativeIdentity,
            _limits: &ComputerUseLimits,
        ) -> ComputerResult<RawMacObservation> {
            if let Some(code) = *self.observation_error.lock() {
                return Err(ComputerError::new(code, "fixture observation failed"));
            }
            let actual_identity = self
                .observation_identity
                .lock()
                .clone()
                .unwrap_or_else(|| identity.clone());
            let frame = *self.observation_frame.lock();
            let (pixel_width, pixel_height) = *self.pixel_dimensions.lock();
            Ok(RawMacObservation {
                identity: actual_identity,
                captured_at: Utc::now(),
                frame,
                pixel_width,
                pixel_height,
                privacy_redacted: true,
                screenshot_png: fake_png(pixel_width, pixel_height),
                nodes: vec![
                    RawMacSemanticNode {
                        role: "AXButton".into(),
                        subrole: None,
                        label: Some(self.content_label.lock().clone()),
                        value: None,
                        frame: Some(ObservationGeometry {
                            x: frame.x + 1.0,
                            y: frame.y + 1.0,
                            width: 1.0,
                            height: 1.0,
                            scale_factor: 1.0,
                        }),
                        enabled: true,
                        focused: false,
                        sensitivity: Sensitivity::None,
                        actions: vec![RawMacSemanticAction::Invoke],
                    },
                    RawMacSemanticNode {
                        role: "AXSecureTextField".into(),
                        subrole: None,
                        label: Some("Password".into()),
                        value: Some("must-not-escape".into()),
                        frame: Some(ObservationGeometry {
                            x: frame.x + 2.0,
                            y: frame.y + 2.0,
                            width: 1.0,
                            height: 1.0,
                            scale_factor: 1.0,
                        }),
                        enabled: true,
                        focused: true,
                        sensitivity: Sensitivity::None,
                        actions: vec![RawMacSemanticAction::SetValue],
                    },
                    RawMacSemanticNode {
                        role: "AXTextField".into(),
                        subrole: None,
                        label: Some("Project label".into()),
                        value: Some(self.text_value.lock().clone()),
                        frame: Some(ObservationGeometry {
                            x: frame.x + 3.0,
                            y: frame.y + 2.0,
                            width: 0.5,
                            height: 1.0,
                            scale_factor: 1.0,
                        }),
                        enabled: true,
                        focused: false,
                        sensitivity: Sensitivity::None,
                        actions: vec![RawMacSemanticAction::SetValue],
                    },
                ],
                nodes_truncated: *self.nodes_truncated.lock(),
                sensitivity: Sensitivity::None,
            })
        }

        fn supports_measured_background(&self) -> bool {
            true
        }

        async fn act(
            &self,
            identity: &MacNativeIdentity,
            request: &RawMacActionRequest,
        ) -> ComputerResult<ActionOutcome> {
            if request.cancellation.is_cancelled() {
                return Err(ComputerError::new(
                    ComputerErrorCode::Interrupted,
                    "fixture action was cancelled before dispatch",
                ));
            }
            if let Some(code) = *self.action_error.lock() {
                return Err(ComputerError::new(code, "fixture action failed"));
            }
            let targets = self.targets.lock();
            let target = targets
                .iter()
                .find(|target| &target.identity == identity)
                .ok_or_else(|| {
                    ComputerError::new(ComputerErrorCode::TargetChanged, "fixture target changed")
                })?;
            let focus_invalid = match request.execution_mode {
                MacSemanticExecutionMode::ForegroundRequired => {
                    !matches!(&request.action, ComputerAction::ActivateTarget) && !target.active
                }
                MacSemanticExecutionMode::MeasuredBackground => {
                    target.active || !matches!(&request.action, ComputerAction::SetValue { .. })
                }
            };
            if target.frame != request.target_frame || focus_invalid {
                return Err(ComputerError::new(
                    ComputerErrorCode::TargetChanged,
                    "fixture target geometry or focus changed",
                ));
            }
            drop(targets);
            if self.block_action_before_dispatch.load(Ordering::SeqCst) {
                self.action_entered.notify_one();
                self.release_action.notified().await;
            }
            if request.cancellation.is_cancelled() {
                return Err(ComputerError::new(
                    ComputerErrorCode::Interrupted,
                    "fixture action was cancelled before dispatch",
                ));
            }
            if request.execution_mode == MacSemanticExecutionMode::MeasuredBackground
                && self.background_interference.load(Ordering::SeqCst)
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::UncertainOutcome,
                    "fixture background dispatch changed user interaction state",
                ));
            }
            self.action_requests.lock().push(request.clone());
            if let Some(postcondition) = *self.action_postcondition.lock() {
                return Ok(ActionOutcome::bounded(
                    "Fixture postcondition result",
                    Some(postcondition),
                ));
            }
            Ok(match &request.action {
                ComputerAction::ActivateTarget => {
                    ActionOutcome::bounded("Activated fixture target", Some(true))
                }
                ComputerAction::SetValue { text, .. } => {
                    if request
                        .expected_element
                        .as_ref()
                        .and_then(|element| element.label.as_deref())
                        == Some("Project label")
                    {
                        *self.text_value.lock() = text.clone();
                    }
                    ActionOutcome::bounded("Verified fixture mutation", Some(true))
                }
                ComputerAction::Select { .. } => {
                    ActionOutcome::bounded("Verified fixture mutation", Some(true))
                }
                ComputerAction::Invoke { .. } | ComputerAction::Scroll { .. } => {
                    ActionOutcome::bounded("Completed fixture semantic action", None)
                }
                _ => unreachable!("backend rejects non-semantic actions before source dispatch"),
            })
        }
    }

    fn fixture_target(bundle_id: &str, window_id: u32, x: f64) -> RawMacTarget {
        RawMacTarget {
            identity: MacNativeIdentity {
                window_id,
                process_id: 100,
                bundle_id: bundle_id.into(),
            },
            application_name: "Demo App".into(),
            frame: ObservationGeometry {
                x,
                y: 40.0,
                width: 4.0,
                height: 4.0,
                scale_factor: 1.0,
            },
            on_screen: true,
            active: true,
            minimized: false,
        }
    }

    fn fake_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = ONE_PIXEL_PNG.to_vec();
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        png
    }

    #[tokio::test]
    async fn selection_is_one_use_and_hard_denied_apps_are_absent() {
        let source = Arc::new(FixtureSource::granted());
        source
            .targets
            .lock()
            .push(fixture_target("com.apple.loginwindow", 7, 0.0));
        let platform = MacOsObservationPlatform::with_source(source);
        let targets = platform.list_targets().await.unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target.display_name, "Demo App");
        assert!(!targets[0].target.display_name.contains("window"));
        let token = targets[0].selection_token.clone();
        platform.bind_target(&token).await.unwrap();
        assert_eq!(
            platform.bind_target(&token).await.unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            platform.bind_target("forged-token").await.unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[tokio::test]
    async fn native_cancel_signals_inflight_action_without_waiting_for_action_gate() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let run_id = Uuid::new_v4().to_string();
        let observation = backend
            .observe(
                &run_id,
                "cancel-observation",
                &candidate.target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        source
            .block_action_before_dispatch
            .store(true, Ordering::SeqCst);
        let action_backend = backend.clone();
        let action_run_id = run_id.clone();
        let action_observation = observation.clone();
        let action = tokio::spawn(async move {
            action_backend
                .act_if_current(
                    &action_run_id,
                    &action_observation,
                    &ComputerAction::Invoke {
                        element_id: action_observation.elements[0].element_id.clone(),
                    },
                )
                .await
        });
        source.action_entered.notified().await;

        tokio::time::timeout(StdDuration::from_secs(1), backend.cancel(&run_id))
            .await
            .expect("cancel must not wait behind the native action gate")
            .unwrap();
        assert!(
            !action.is_finished(),
            "the source remains blocked so cancel returning proves an out-of-band signal"
        );
        source.release_action.notify_one();
        assert_eq!(
            action.await.unwrap().unwrap_err().code,
            ComputerErrorCode::Interrupted
        );
        assert!(source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn native_cancel_is_scoped_to_the_exact_run() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let run_id = Uuid::new_v4().to_string();
        let unrelated_run_id = Uuid::new_v4().to_string();
        let observation = backend
            .observe(
                &run_id,
                "scoped-cancel-observation",
                &candidate.target,
                &ComputerUseLimits::default(),
            )
            .await
            .unwrap();
        source
            .block_action_before_dispatch
            .store(true, Ordering::SeqCst);
        let action_backend = backend.clone();
        let action_run_id = run_id.clone();
        let action_observation = observation.clone();
        let action = tokio::spawn(async move {
            action_backend
                .act_if_current(
                    &action_run_id,
                    &action_observation,
                    &ComputerAction::Invoke {
                        element_id: action_observation.elements[0].element_id.clone(),
                    },
                )
                .await
        });
        source.action_entered.notified().await;

        tokio::time::timeout(StdDuration::from_secs(1), backend.cancel(&unrelated_run_id))
            .await
            .expect("unrelated cancellation must remain out of band")
            .unwrap();
        assert!(!action.is_finished());
        source.release_action.notify_one();
        action.await.unwrap().unwrap();
        assert_eq!(source.action_requests.lock().len(), 1);
    }

    #[tokio::test]
    async fn native_service_takeover_returns_before_blocked_source_action() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let directory = tempfile::tempdir().unwrap();
        let service = Arc::new(
            platform
                .bind_target_service(
                    &candidate.selection_token,
                    ComputerStore::open(directory.path().join("computer-use")).unwrap(),
                )
                .await
                .unwrap(),
        );
        let owner_session_id = Uuid::new_v4();
        let caller =
            crate::computer_use::ComputerAuthorityToken::local_operator(owner_session_id).unwrap();
        let run = service
            .create_run(
                "create-native-takeover",
                &caller,
                None,
                candidate.target,
                ComputerUseLimits::default(),
            )
            .unwrap();
        let now = Utc::now();
        let run = service
            .authorize(
                "authorize-native-takeover",
                &caller,
                &run.run_id,
                run.version,
                crate::computer_use::ActionGrant::for_run(
                    &run,
                    BTreeSet::from([crate::computer_use::ActionClass::Semantic]),
                    now,
                    now + chrono::Duration::minutes(5),
                    Some(1),
                ),
            )
            .unwrap();
        let observation = service
            .observe("observe-native-takeover", &caller, &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        source
            .block_action_before_dispatch
            .store(true, Ordering::SeqCst);
        let action_service = service.clone();
        let action_caller = caller.clone();
        let action_run_id = run.run_id.clone();
        let action_observation = observation.clone();
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-native-takeover",
                    &action_caller,
                    &action_run_id,
                    current.version,
                    &action_observation.observation_id,
                    ComputerAction::Invoke {
                        element_id: action_observation.elements[0].element_id.clone(),
                    },
                )
                .await
        });
        source.action_entered.notified().await;

        let taken_over = tokio::time::timeout(
            StdDuration::from_secs(1),
            service.take_over("take-over-native-action", &caller, &run.run_id),
        )
        .await
        .expect("takeover must not wait for the blocked native source")
        .unwrap();
        assert_eq!(
            taken_over.control_disposition,
            crate::computer_use::ComputerControlDisposition::OperatorTakeover
        );
        assert_eq!(taken_over.action_count, 0);
        assert!(!action.is_finished());

        source.release_action.notify_one();
        assert_eq!(
            action.await.unwrap().unwrap_err().code,
            ComputerErrorCode::Interrupted
        );
        let final_run = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            final_run.control_disposition,
            crate::computer_use::ComputerControlDisposition::OperatorTakeover
        );
        assert_eq!(final_run.action_count, 0);
        assert!(source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn measured_background_text_entry_is_reversible_one_use_and_exact() {
        let source = Arc::new(FixtureSource::granted());
        source.targets.lock()[0].active = false;
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let receipt = platform
            .measure_background_text_entry(
                &candidate.selection_token,
                "Project label",
                "background-probe-value",
                true,
            )
            .await
            .unwrap();
        assert_eq!(receipt.target, candidate.target);
        assert_eq!(
            receipt.supported_action_classes,
            BTreeSet::from([crate::computer_use::ActionClass::TextEntry])
        );
        assert_eq!(source.text_value.lock().as_str(), "public-demo-value");
        let requests = source.action_requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            request.execution_mode == MacSemanticExecutionMode::MeasuredBackground
        }));
        drop(requests);

        let directory = tempfile::tempdir().unwrap();
        let service = platform
            .bind_measured_background_target_service(
                &candidate.selection_token,
                &receipt.measurement_token,
                ComputerStore::open(directory.path().join("computer-use")).unwrap(),
            )
            .await
            .unwrap();
        let owner_session_id = Uuid::new_v4();
        let caller =
            crate::computer_use::ComputerAuthorityToken::local_operator(owner_session_id).unwrap();
        let run = service
            .create_run(
                "create-background-run",
                &caller,
                None,
                candidate.target,
                ComputerUseLimits::default(),
            )
            .unwrap();
        assert_eq!(
            run.capability_proof.tier(),
            ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        );
        assert!(!run.capability_proof.semantic_actions());
        assert!(run.capability_proof.text_entry());
        let now = Utc::now();
        let run = service
            .authorize(
                "authorize-background-run",
                &caller,
                &run.run_id,
                run.version,
                crate::computer_use::ActionGrant::for_run(
                    &run,
                    BTreeSet::from([crate::computer_use::ActionClass::TextEntry]),
                    now,
                    now + chrono::Duration::minutes(5),
                    Some(1),
                ),
            )
            .unwrap();
        let observation = service
            .observe("observe-background-run", &caller, &run.run_id, run.version)
            .await
            .unwrap();
        let element_id = observation
            .elements
            .iter()
            .find(|element| element.label.as_deref() == Some("Project label"))
            .unwrap()
            .element_id
            .clone();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let outcome = service
            .act(
                "act-background-run",
                &caller,
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id,
                    text: "agent-background-value".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.expected_postcondition_met, Some(true));
        assert_eq!(source.text_value.lock().as_str(), "agent-background-value");
        assert_eq!(
            platform
                .bind_measured_background_target_service(
                    &candidate.selection_token,
                    &receipt.measurement_token,
                    ComputerStore::open(directory.path().join("second-use")).unwrap(),
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[tokio::test]
    async fn background_probe_requires_disposable_ack_and_fails_closed_on_interference() {
        let source = Arc::new(FixtureSource::granted());
        source.targets.lock()[0].active = false;
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        assert_eq!(
            platform
                .measure_background_text_entry(
                    &candidate.selection_token,
                    "Project label",
                    "background-probe-value",
                    false,
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
        source.background_interference.store(true, Ordering::SeqCst);
        assert_eq!(
            platform
                .measure_background_text_entry(
                    &candidate.selection_token,
                    "Project label",
                    "background-probe-value",
                    true,
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::UncertainOutcome
        );
        assert_eq!(source.text_value.lock().as_str(), "public-demo-value");
        assert!(source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn background_probe_rejects_foreground_hidden_minimized_and_secure_targets() {
        let configurations: [fn(&mut RawMacTarget); 3] = [
            |target: &mut RawMacTarget| target.active = true,
            |target: &mut RawMacTarget| target.on_screen = false,
            |target: &mut RawMacTarget| target.minimized = true,
        ];
        for configure in configurations {
            let source = Arc::new(FixtureSource::granted());
            {
                let mut targets = source.targets.lock();
                targets[0].active = false;
                configure(&mut targets[0]);
            }
            let platform = MacOsObservationPlatform::with_source(source);
            let candidate = platform.list_targets().await.unwrap().remove(0);
            assert_eq!(
                platform
                    .measure_background_text_entry(
                        &candidate.selection_token,
                        "Project label",
                        "background-probe-value",
                        true,
                    )
                    .await
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenTarget
            );
        }

        let source = Arc::new(FixtureSource::granted());
        source.targets.lock()[0].active = false;
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        assert_eq!(
            platform
                .measure_background_text_entry(
                    &candidate.selection_token,
                    "Password",
                    "must-not-be-written",
                    true,
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
        assert_eq!(source.text_value.lock().as_str(), "public-demo-value");
        assert!(source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn measured_background_service_denies_foreground_transition() {
        let source = Arc::new(FixtureSource::granted());
        source.targets.lock()[0].active = false;
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let receipt = platform
            .measure_background_text_entry(
                &candidate.selection_token,
                "Project label",
                "background-probe-value",
                true,
            )
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let service = platform
            .bind_measured_background_target_service(
                &candidate.selection_token,
                &receipt.measurement_token,
                ComputerStore::open(directory.path().join("computer-use")).unwrap(),
            )
            .await
            .unwrap();
        let owner_session_id = Uuid::new_v4();
        let caller =
            crate::computer_use::ComputerAuthorityToken::local_operator(owner_session_id).unwrap();
        let run = service
            .create_run(
                "create-background-transition-run",
                &caller,
                None,
                candidate.target,
                ComputerUseLimits::default(),
            )
            .unwrap();
        let now = Utc::now();
        let run = service
            .authorize(
                "authorize-background-transition-run",
                &caller,
                &run.run_id,
                run.version,
                crate::computer_use::ActionGrant::for_run(
                    &run,
                    BTreeSet::from([crate::computer_use::ActionClass::TextEntry]),
                    now,
                    now + chrono::Duration::minutes(5),
                    Some(1),
                ),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-background-transition-run",
                &caller,
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let element_id = observation
            .elements
            .iter()
            .find(|element| element.label.as_deref() == Some("Project label"))
            .unwrap()
            .element_id
            .clone();
        source.targets.lock()[0].active = true;
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            service
                .act(
                    "act-after-foreground-transition",
                    &caller,
                    &run.run_id,
                    current.version,
                    &observation.observation_id,
                    ComputerAction::SetValue {
                        element_id,
                        text: "must-not-dispatch".into(),
                    },
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
        assert_eq!(source.text_value.lock().as_str(), "public-demo-value");
        assert_eq!(source.action_requests.lock().len(), 2);
    }

    #[tokio::test]
    async fn host_bound_macos_service_is_foreground_semantic_only() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source);
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let directory = tempfile::tempdir().unwrap();
        let service = platform
            .bind_target_service(
                &candidate.selection_token,
                ComputerStore::open(directory.path().join("computer-use")).unwrap(),
            )
            .await
            .unwrap();
        let capabilities = service.capabilities();
        assert_eq!(
            capabilities.backend_id,
            crate::computer_use::MACOS_NATIVE_BACKEND_ID
        );
        assert_eq!(
            capabilities.tier,
            ComputerCapabilityTier::ForegroundSemantic
        );
        assert!(!capabilities.pointer_fallback);
        assert!(!capabilities.key_chords);
        assert!(!capabilities.proof.isolated_input_is_dispatchable());
    }

    #[tokio::test]
    async fn refreshing_targets_invalidates_prior_selection_snapshot() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source);
        let old_token = platform.list_targets().await.unwrap()[0]
            .selection_token
            .clone();
        let current_token = platform.list_targets().await.unwrap()[0]
            .selection_token
            .clone();

        assert_eq!(
            platform.bind_target(&old_token).await.unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        platform.bind_target(&current_token).await.unwrap();
    }

    #[tokio::test]
    async fn failed_target_refresh_also_invalidates_prior_selection_snapshot() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let old_token = platform.list_targets().await.unwrap()[0]
            .selection_token
            .clone();
        *source.list_error.lock() = Some(ComputerErrorCode::BackendFailure);

        assert_eq!(
            platform.list_targets().await.unwrap_err().code,
            ComputerErrorCode::BackendFailure
        );
        assert_eq!(
            platform.bind_target(&old_token).await.unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn rust_and_native_hard_deny_lists_stay_aligned() {
        let native = include_str!("macos_native_shim.m").to_ascii_lowercase();
        for bundle_id in HARD_DENIED_BUNDLE_IDS {
            assert!(
                native.contains(bundle_id),
                "native deny list omits {bundle_id}"
            );
        }
    }

    #[test]
    fn native_macos_windows_share_one_host_global_foreground_conflict_domain() {
        let mail = MacNativeIdentity {
            window_id: 11,
            process_id: 101,
            bundle_id: "com.apple.mail".into(),
        };
        let safari = MacNativeIdentity {
            window_id: 22,
            process_id: 202,
            bundle_id: "com.apple.Safari".into(),
        };
        assert_eq!(mail.physical_input_domain(), safari.physical_input_domain());
        assert_eq!(
            mail.physical_input_domain(),
            macos_native_physical_input_domain()
        );
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::computer_use::ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let interned_mail = store
            .intern_physical_domain(&mail.physical_input_domain())
            .unwrap();
        let interned_safari = store
            .intern_physical_domain(&safari.physical_input_domain())
            .unwrap();
        assert_eq!(interned_mail.binding, interned_safari.binding);
        assert_eq!(crate::computer_use::FOREGROUND_CONFLICT_DOMAIN_CAPACITY, 1);
    }

    #[tokio::test]
    async fn secure_values_are_removed_and_evidence_is_exactly_scoped() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source);
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let run_id = Uuid::new_v4().to_string();
        let observation = backend
            .observe(
                &run_id,
                "secure-evidence-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(observation.geometry.x, -1200.0);
        assert_eq!(observation.elements.len(), 1);
        assert_eq!(observation.elements[0].role, "AXButton");
        assert!(!serde_json::to_string(&observation)
            .unwrap()
            .contains("must-not-escape"));
        assert_eq!(observation.elements[0].bounds.unwrap().x, 1.0);
        let evidence = observation.screenshot.unwrap();
        assert_eq!(
            backend
                .read_evidence(&run_id, &evidence.asset_id)
                .await
                .unwrap()
                .unwrap(),
            ONE_PIXEL_PNG
        );
        assert!(backend
            .read_evidence("other-run", &evidence.asset_id)
            .await
            .unwrap()
            .is_none());
        backend.cancel(&run_id).await.unwrap();
        assert!(backend
            .read_evidence(&run_id, &evidence.asset_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn successive_observations_rotate_element_ids_and_evidence() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source);
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let run_id = Uuid::new_v4().to_string();
        let first = backend
            .observe(
                &run_id,
                "rotating-observation-1",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let first_element = first.elements[0].element_id.clone();
        let first_evidence = first.screenshot.unwrap();
        let second = backend
            .observe(
                &run_id,
                "rotating-observation-2",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let second_evidence = second.screenshot.unwrap();

        assert_ne!(first_element, second.elements[0].element_id);
        assert_ne!(first_evidence.asset_id, second_evidence.asset_id);
        assert!(backend
            .read_evidence(&run_id, &first_evidence.asset_id)
            .await
            .unwrap()
            .is_none());
        assert!(backend
            .read_evidence(&run_id, &second_evidence.asset_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn truncated_semantic_walk_never_exposes_screenshot_evidence() {
        let source = Arc::new(FixtureSource::granted());
        *source.nodes_truncated.lock() = true;
        let platform = MacOsObservationPlatform::with_source(source);
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let observation = backend
            .observe(
                "run",
                "truncated-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();

        assert!(observation.elements_truncated);
        assert!(observation.screenshot.is_none());
    }

    #[tokio::test]
    async fn target_identity_change_fails_closed() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        *source.observation_identity.lock() = Some(MacNativeIdentity {
            window_id: 99,
            process_id: 100,
            bundle_id: "com.example.demo".into(),
        });
        assert_eq!(
            backend
                .observe(
                    "run",
                    "changed-target-observation",
                    &candidate.target,
                    &Default::default(),
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetChanged
        );
    }

    #[tokio::test]
    async fn permission_states_are_distinct() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        source.status.lock().screen_recording = ComputerPermissionStatus::Missing;
        assert_eq!(
            platform.list_targets().await.unwrap_err().code,
            ComputerErrorCode::PermissionRequired
        );
        source.status.lock().screen_recording = ComputerPermissionStatus::Denied;
        assert_eq!(
            platform.list_targets().await.unwrap_err().code,
            ComputerErrorCode::PermissionDenied
        );
        source.status.lock().screen_recording = ComputerPermissionStatus::PromptPending;
        assert_eq!(
            platform.list_targets().await.unwrap_err().code,
            ComputerErrorCode::PermissionRequired
        );
        source.status.lock().screen_recording = ComputerPermissionStatus::Revoked;
        assert_eq!(
            platform.list_targets().await.unwrap_err().code,
            ComputerErrorCode::PermissionRevoked
        );
    }

    #[tokio::test]
    async fn retina_and_moved_window_geometry_is_normalized_from_current_capture() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        *source.observation_frame.lock() = ObservationGeometry {
            x: -2000.0,
            y: 200.0,
            width: 640.0,
            height: 480.0,
            scale_factor: 1.0,
        };
        *source.pixel_dimensions.lock() = (1280, 960);

        let observation = backend
            .observe(
                "run",
                "geometry-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(observation.geometry.x, -2000.0);
        assert_eq!(observation.geometry.y, 200.0);
        assert_eq!(observation.geometry.scale_factor, 2.0);
        assert_eq!(observation.elements[0].bounds.unwrap().x, 1.0);
    }

    #[tokio::test]
    async fn closed_hidden_and_changed_targets_remain_distinct() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let first = platform.list_targets().await.unwrap().remove(0);
        source.targets.lock().clear();
        assert_eq!(
            platform
                .bind_target(&first.selection_token)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetClosed
        );

        source
            .targets
            .lock()
            .push(fixture_target("com.example.demo", 42, -1200.0));
        let second = platform.list_targets().await.unwrap().remove(0);
        let backend = platform.bind_target(&second.selection_token).await.unwrap();
        *source.observation_error.lock() = Some(ComputerErrorCode::TargetClosed);
        assert_eq!(
            backend
                .observe(
                    "run",
                    "closed-target-observation",
                    &second.target,
                    &Default::default(),
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetClosed
        );
        *source.observation_error.lock() = Some(ComputerErrorCode::TargetChanged);
        assert_eq!(
            backend
                .observe(
                    "run",
                    "changed-target-observation",
                    &second.target,
                    &Default::default(),
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetChanged
        );
    }

    #[tokio::test]
    async fn semantic_backend_binds_actions_to_exact_fresh_elements() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let first = backend
            .observe(
                "run",
                "semantic-observation-1",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let activated = backend
            .act("run", &first, &ComputerAction::ActivateTarget)
            .await
            .unwrap();
        assert_eq!(activated.expected_postcondition_met, Some(true));

        let second = backend
            .observe(
                "run",
                "semantic-observation-2",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let element_id = second.elements[0].element_id.clone();
        backend
            .act(
                "run",
                &second,
                &ComputerAction::Invoke {
                    element_id: element_id.clone(),
                },
            )
            .await
            .unwrap();
        {
            let requests = source.action_requests.lock();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].element_index, Some(0));
            assert_eq!(
                requests[1]
                    .expected_element
                    .as_ref()
                    .unwrap()
                    .label
                    .as_deref(),
                Some("Continue")
            );
        }

        assert_eq!(
            backend
                .act("run", &second, &ComputerAction::Invoke { element_id },)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        assert!(backend.capabilities().semantic_actions);
        assert!(backend.capabilities().text_entry);
        assert!(!backend.capabilities().pointer_fallback);
        assert!(!backend.capabilities().key_chords);
        assert_eq!(
            backend.capabilities().tier,
            crate::computer_use::ComputerCapabilityTier::ForegroundSemantic
        );
        assert!(!backend
            .capabilities()
            .proof
            .isolated_input_is_dispatchable());
        assert_eq!(
            backend.physical_input_domain(),
            macos_native_physical_input_domain()
        );
        assert!(!candidate.target.window_id.contains("macos-window-"));
        assert!(!candidate
            .target
            .window_id
            .chars()
            .all(|ch| ch.is_ascii_digit()));
    }

    #[tokio::test]
    async fn truncated_tree_and_nonsemantic_input_never_reach_native_dispatch() {
        let source = Arc::new(FixtureSource::granted());
        *source.nodes_truncated.lock() = true;
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let observation = backend
            .observe(
                "run",
                "truncated-action-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::ActivateTarget)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::SensitiveSurface
        );
        assert!(source.action_requests.lock().is_empty());

        let semantic_source = Arc::new(FixtureSource::granted());
        let semantic_platform = MacOsObservationPlatform::with_source(semantic_source.clone());
        let semantic_candidate = semantic_platform.list_targets().await.unwrap().remove(0);
        let semantic_backend = semantic_platform
            .bind_target(&semantic_candidate.selection_token)
            .await
            .unwrap();
        let semantic_observation = semantic_backend
            .observe(
                "semantic-run",
                "nonsemantic-input-observation",
                &semantic_candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            semantic_backend
                .act(
                    "semantic-run",
                    &semantic_observation,
                    &ComputerAction::Wait { millis: 1 },
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
        assert!(semantic_source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn failed_postcondition_consumes_native_observation() {
        let source = Arc::new(FixtureSource::granted());
        *source.action_postcondition.lock() = Some(false);
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let observation = backend
            .observe(
                "run",
                "postcondition-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();

        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::ActivateTarget)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::BackendFailure
        );
        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::ActivateTarget)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        assert_eq!(source.action_requests.lock().len(), 1);
    }

    #[tokio::test]
    async fn permission_revocation_at_dispatch_consumes_the_native_observation() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let observation = backend
            .observe(
                "run",
                "revocation-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        source.status.lock().accessibility = ComputerPermissionStatus::Revoked;

        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::ActivateTarget)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::PermissionRevoked
        );
        source.status.lock().accessibility = ComputerPermissionStatus::Granted;
        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::ActivateTarget)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        assert!(source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn geometry_and_focus_drift_fail_before_native_input_and_consume_the_frame() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let observation = backend
            .observe(
                "run",
                "geometry-drift-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let element_id = observation.elements[0].element_id.clone();
        source.targets.lock()[0].frame.x += 10.0;

        assert_eq!(
            backend
                .act(
                    "run",
                    &observation,
                    &ComputerAction::Invoke {
                        element_id: element_id.clone(),
                    },
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetChanged
        );
        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::Invoke { element_id },)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        assert!(source.action_requests.lock().is_empty());

        source.targets.lock()[0].frame.x -= 10.0;
        let focused = backend
            .observe(
                "run",
                "focus-drift-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let focused_element = focused.elements[0].element_id.clone();
        source.targets.lock()[0].active = false;
        assert_eq!(
            backend
                .act(
                    "run",
                    &focused,
                    &ComputerAction::Invoke {
                        element_id: focused_element,
                    },
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetChanged
        );
        assert!(source.action_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn same_shape_content_drift_fails_before_native_input() {
        let source = Arc::new(FixtureSource::granted());
        let platform = MacOsObservationPlatform::with_source(source.clone());
        let candidate = platform.list_targets().await.unwrap().remove(0);
        let backend = platform
            .bind_target(&candidate.selection_token)
            .await
            .unwrap();
        let observation = backend
            .observe(
                "run",
                "content-drift-observation",
                &candidate.target,
                &Default::default(),
            )
            .await
            .unwrap();
        let element_id = observation.elements[0].element_id.clone();
        *source.content_label.lock() = "Submit".into();
        assert_eq!(
            backend
                .act("run", &observation, &ComputerAction::Invoke { element_id },)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );
        assert!(source.action_requests.lock().is_empty());
    }
}
