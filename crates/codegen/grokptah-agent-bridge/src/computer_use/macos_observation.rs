use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::platform::{
    ComputerObservationPlatform, ComputerPermission, ComputerPermissionStatus,
    ComputerPlatformStatus, ComputerTargetCandidate,
};
use super::types::{
    ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities, ComputerError,
    ComputerErrorCode, ComputerObservation, ComputerResult, ComputerTarget, ComputerUseLimits,
    EvidenceRef, ObservationGeometry, SemanticAction, SemanticElement, Sensitivity,
    MAX_LABEL_BYTES,
};

const MAX_TARGET_CANDIDATES: usize = 128;
const SELECTION_LEASE_TTL: StdDuration = StdDuration::from_secs(120);
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
pub(crate) struct RawMacActionRequest {
    pub target_frame: ObservationGeometry,
    pub element_index: Option<usize>,
    pub expected_element: Option<RawMacSemanticNode>,
    pub action: ComputerAction,
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

#[derive(Debug)]
pub struct MacOsObservationPlatform {
    source: Arc<dyn MacObservationSource>,
    leases: Mutex<HashMap<String, TargetLease>>,
}

impl MacOsObservationPlatform {
    pub(crate) fn with_source(source: Arc<dyn MacObservationSource>) -> Self {
        Self {
            source,
            leases: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn new_native() -> ComputerResult<Self> {
        Ok(Self::with_source(Arc::new(
            super::macos_native::NativeMacObservationSource::new()?,
        )))
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
                    window_id: format!("macos-window-{}", raw.identity.window_id),
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

    async fn bind_target(&self, selection_token: &str) -> ComputerResult<Arc<dyn ComputerBackend>> {
        super::types::validate_id("selection_token", selection_token)?;
        let lease = self.leases.lock().remove(selection_token).ok_or_else(|| {
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
        require_platform_ready(&self.status())?;
        let current = self
            .source
            .revalidate_target(&lease.native.identity)
            .await?;
        validate_raw_target(&current)?;
        if current.identity != lease.native.identity {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "selected macOS target identity changed",
            ));
        }
        Ok(Arc::new(MacOsObservationBackend {
            source: self.source.clone(),
            target: lease.candidate.target,
            native_identity: lease.native.identity,
            sequence: Mutex::new(0),
            observation_gate: tokio::sync::Mutex::new(()),
            last_capture_started: Mutex::new(None),
            cancellation_epoch: AtomicU64::new(0),
            action_gate: tokio::sync::Mutex::new(()),
            action_snapshot: Mutex::new(None),
            evidence: EvidenceVault::default(),
        }))
    }
}

#[derive(Debug, Clone)]
struct MacActionSnapshot {
    observation_id: String,
    target_frame: ObservationGeometry,
    nodes: Vec<RawMacSemanticNode>,
    nodes_truncated: bool,
}

#[derive(Debug)]
struct MacOsObservationBackend {
    source: Arc<dyn MacObservationSource>,
    target: ComputerTarget,
    native_identity: MacNativeIdentity,
    sequence: Mutex<u64>,
    observation_gate: tokio::sync::Mutex<()>,
    last_capture_started: Mutex<Option<Instant>>,
    cancellation_epoch: AtomicU64,
    action_gate: tokio::sync::Mutex<()>,
    action_snapshot: Mutex<Option<MacActionSnapshot>>,
    evidence: EvidenceVault,
}

#[async_trait]
impl ComputerBackend for MacOsObservationBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "macos_accessibility_semantic".into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: false,
            pointer_fallback: false,
        }
    }

    async fn observe(
        &self,
        run_id: &str,
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
        let epoch = self.cancellation_epoch.load(Ordering::SeqCst);
        let raw = self.source.observe(&self.native_identity, limits).await?;
        if self.cancellation_epoch.load(Ordering::SeqCst) != epoch {
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
            observation_id: String::new(),
            target_frame: raw.frame,
            nodes: raw.nodes.clone(),
            nodes_truncated: raw.nodes_truncated,
        };
        let observation = normalize_observation(
            run_id,
            &self.target,
            raw,
            limits,
            &self.sequence,
            &self.evidence,
        )?;
        if self.cancellation_epoch.load(Ordering::SeqCst) != epoch {
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
        _run_id: &str,
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
        let epoch = self.cancellation_epoch.load(Ordering::SeqCst);
        let snapshot = self
            .action_snapshot
            .lock()
            .clone()
            .filter(|snapshot| snapshot.observation_id == observation.observation_id)
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
        let (element_index, expected_element) =
            action_element(snapshot.nodes.as_slice(), observation, action)?;
        let request = RawMacActionRequest {
            target_frame: snapshot.target_frame,
            element_index,
            expected_element,
            action: action.clone(),
        };
        if self.cancellation_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "macOS action was cancelled before dispatch",
            ));
        }
        // Consume the attestation before crossing the native mutation
        // boundary. A target or postcondition error after dispatch is an
        // uncertain physical outcome and must never make this frame reusable.
        *self.action_snapshot.lock() = None;
        let outcome = self.source.act(&self.native_identity, &request).await?;
        if self.cancellation_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "macOS action completion lost to local takeover",
            ));
        }
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
        let _action_guard = self.action_gate.lock().await;
        self.cancellation_epoch.fetch_add(1, Ordering::SeqCst);
        *self.action_snapshot.lock() = None;
        self.evidence.remove_run(run_id);
        Ok(())
    }
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
    let observation_id = format!("macos-observation-{}-{}", current_sequence, Uuid::new_v4());
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
        let secure_role = role.to_ascii_lowercase().contains("secure")
            || node
                .subrole
                .as_ref()
                .is_some_and(|subrole| subrole.to_ascii_lowercase().contains("secure"));
        let sensitivity = if secure_role {
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
        observation_id,
        sequence: current_sequence,
        target: target.clone(),
        captured_at: raw.captured_at,
        geometry,
        screenshot,
        elements,
        elements_truncated,
        sensitivity: raw.sensitivity,
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
                        label: Some("Continue".into()),
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
                ],
                nodes_truncated: *self.nodes_truncated.lock(),
                sensitivity: Sensitivity::None,
            })
        }

        async fn act(
            &self,
            identity: &MacNativeIdentity,
            request: &RawMacActionRequest,
        ) -> ComputerResult<ActionOutcome> {
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
            if target.frame != request.target_frame
                || (!matches!(&request.action, ComputerAction::ActivateTarget) && !target.active)
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::TargetChanged,
                    "fixture target geometry or focus changed",
                ));
            }
            drop(targets);
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
                ComputerAction::SetValue { .. } | ComputerAction::Select { .. } => {
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
            .observe(&run_id, &candidate.target, &Default::default())
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
            .observe(&run_id, &candidate.target, &Default::default())
            .await
            .unwrap();
        let first_element = first.elements[0].element_id.clone();
        let first_evidence = first.screenshot.unwrap();
        let second = backend
            .observe(&run_id, &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
                .observe("run", &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
                .observe("run", &second.target, &Default::default())
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetClosed
        );
        *source.observation_error.lock() = Some(ComputerErrorCode::TargetChanged);
        assert_eq!(
            backend
                .observe("run", &second.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
            .await
            .unwrap();
        let activated = backend
            .act("run", &first, &ComputerAction::ActivateTarget)
            .await
            .unwrap();
        assert_eq!(activated.expected_postcondition_met, Some(true));

        let second = backend
            .observe("run", &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
            .observe("run", &candidate.target, &Default::default())
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
}
