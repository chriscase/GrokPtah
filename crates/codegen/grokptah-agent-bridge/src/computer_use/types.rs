use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
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
}

impl ComputerObservation {
    pub fn validate(&self, limits: &ComputerUseLimits) -> ComputerResult<()> {
        validate_id("observation_id", &self.observation_id)?;
        self.target.validate()?;
        self.geometry.validate()?;
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
}

impl ActionGrant {
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
        if self.uses_remaining == Some(0) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "grant has no remaining uses",
            ));
        }
        Ok(())
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
        })
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
        let mut run = ComputerRun::new(Uuid::new_v4(), None, target(), Default::default()).unwrap();
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
}
