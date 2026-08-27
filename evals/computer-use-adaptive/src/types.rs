//! Closed evaluation types. Unknown fields fail closed.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const SCENARIO_SCHEMA: &str = "grokptah.cu_eval_scenario.v1";
pub const RESULT_SCHEMA: &str = "grokptah.cu_eval_episode_result.v1";
pub const EVIDENCE_SCHEMA: &str = "grokptah.cu_eval_evidence.v1";
pub const REPORT_SCHEMA: &str = "grokptah.cu_eval_campaign_report.v1";
pub const SOURCE_GATE_SHA: &str = "67e29bd34dc64049432c715c93c2cef2185c63ea";

pub const MAX_STEPS: u32 = 12;
pub const STATIONARITY_WINDOW: usize = 3;
pub const DEFAULT_REPEATS: u32 = 5;
pub const DEFAULT_SEED: u64 = 435_272;
pub const MAX_OBJECTIVE_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileId {
    Economy,
    Balanced,
    HighAssurance,
}

impl ProfileId {
    pub const ALL: [ProfileId; 3] = [
        ProfileId::Economy,
        ProfileId::Balanced,
        ProfileId::HighAssurance,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterId {
    TextOnlyTools,
    WeakMultimodal,
    MalformedOverconfident,
    StationarityLoop,
    FrontierMultimodal,
}

impl AdapterId {
    pub const ALL: [AdapterId; 5] = [
        AdapterId::TextOnlyTools,
        AdapterId::WeakMultimodal,
        AdapterId::MalformedOverconfident,
        AdapterId::StationarityLoop,
        AdapterId::FrontierMultimodal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TextOnlyTools => "text_only_tools",
            Self::WeakMultimodal => "weak_multimodal",
            Self::MalformedOverconfident => "malformed_overconfident",
            Self::StationarityLoop => "stationarity_loop",
            Self::FrontierMultimodal => "frontier_multimodal",
        }
    }

    pub fn capabilities(self) -> ModelCapability {
        match self {
            Self::TextOnlyTools => ModelCapability {
                tools: true,
                vision: false,
                structured_output: true,
            },
            Self::WeakMultimodal => ModelCapability {
                tools: true,
                vision: true,
                structured_output: true,
            },
            Self::MalformedOverconfident => ModelCapability {
                tools: true,
                vision: true,
                structured_output: false,
            },
            Self::StationarityLoop => ModelCapability {
                tools: true,
                vision: false,
                structured_output: true,
            },
            Self::FrontierMultimodal => ModelCapability {
                tools: true,
                vision: true,
                structured_output: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyId {
    UniqueSemanticNoScreenshot,
    DuplicateNamesDisambiguation,
    MissingSemanticsVisualGrounding,
    AxPixelContradictionStale,
    MovingResizedRestartedTarget,
    RepeatedNoopStationarity,
    SensitiveCredentialSystem,
    TakeoverRace,
    TimeoutSendInput,
    SplitSemanticVisual,
    CapabilityDowngrade,
    SurfaceContention,
}

impl FamilyId {
    pub const ALL: [FamilyId; 12] = [
        FamilyId::UniqueSemanticNoScreenshot,
        FamilyId::DuplicateNamesDisambiguation,
        FamilyId::MissingSemanticsVisualGrounding,
        FamilyId::AxPixelContradictionStale,
        FamilyId::MovingResizedRestartedTarget,
        FamilyId::RepeatedNoopStationarity,
        FamilyId::SensitiveCredentialSystem,
        FamilyId::TakeoverRace,
        FamilyId::TimeoutSendInput,
        FamilyId::SplitSemanticVisual,
        FamilyId::CapabilityDowngrade,
        FamilyId::SurfaceContention,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UniqueSemanticNoScreenshot => "unique_semantic_no_screenshot",
            Self::DuplicateNamesDisambiguation => "duplicate_names_disambiguation",
            Self::MissingSemanticsVisualGrounding => "missing_semantics_visual_grounding",
            Self::AxPixelContradictionStale => "ax_pixel_contradiction_stale",
            Self::MovingResizedRestartedTarget => "moving_resized_restarted_target",
            Self::RepeatedNoopStationarity => "repeated_noop_stationarity",
            Self::SensitiveCredentialSystem => "sensitive_credential_system",
            Self::TakeoverRace => "takeover_race",
            Self::TimeoutSendInput => "timeout_send_input",
            Self::SplitSemanticVisual => "split_semantic_visual",
            Self::CapabilityDowngrade => "capability_downgrade",
            Self::SurfaceContention => "surface_contention",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Semantic,
    TextEntry,
    KeyChord,
    PointerFallback,
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
    Space,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum TypedAction {
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
        x: i32,
        y: i32,
        button: PointerButton,
    },
    Wait {
        millis: u64,
    },
}

impl TypedAction {
    pub fn class(&self) -> ActionClass {
        match self {
            Self::ActivateTarget
            | Self::Invoke { .. }
            | Self::Select { .. }
            | Self::Scroll { .. }
            | Self::Wait { .. } => ActionClass::Semantic,
            Self::SetValue { .. } => ActionClass::TextEntry,
            Self::KeyChord { .. } => ActionClass::KeyChord,
            Self::PointerClick { .. } => ActionClass::PointerFallback,
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

    pub fn fingerprint(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "invalid".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ClosedModelOutput {
    Act {
        observation_id: String,
        action: TypedAction,
    },
    Abstain {
        code: String,
        message: String,
    },
    Escalate {
        code: String,
        requested_capability: String,
        message: String,
    },
    Complete {
        postcondition_id: String,
    },
    Malformed {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    Success,
    Abstain,
    Escalate,
    FailClosed,
    Uncertain,
    NoProgress,
}

impl OutcomeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Abstain => "abstain",
            Self::Escalate => "escalate",
            Self::FailClosed => "fail_closed",
            Self::Uncertain => "uncertain",
            Self::NoProgress => "no_progress",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutClass {
    DefinitelyBeforeSend,
    UncertainAfterSend,
    UncertainAfterInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashCut {
    BeforeSend,
    AfterSend,
    AfterInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Queued,
    Granted,
    Dispatching,
    Released,
    Revoked,
    Cancelled,
    Quarantined,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapability {
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
}

impl ModelCapability {
    pub fn underqualified(self) -> bool {
        !self.tools
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Geometry {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x + self.width as i32
            && y < self.y + self.height as i32
    }

    pub fn center(self) -> (i32, i32) {
        (
            self.x + self.width as i32 / 2,
            self.y + self.height as i32 / 2,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct CompactElement {
    pub element_id: String,
    pub stable_key: String,
    pub role: String,
    pub name: String,
    pub context: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub sensitivity: Sensitivity,
    pub advertised_actions: BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Geometry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct FrameRegion {
    pub label: String,
    pub bounds: Geometry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct CompactObservation {
    pub observation_id: String,
    pub sequence: u64,
    pub surface_id: String,
    pub app_id: String,
    pub window_id: String,
    pub generation: u64,
    pub incarnation: u64,
    pub captured_at_ms: u64,
    pub sensitivity: Sensitivity,
    pub ax_pixel_contradiction: bool,
    pub elements: Vec<CompactElement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_regions: Option<Vec<FrameRegion>>,
    pub image_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    SyntheticOnly,
    LiveReusableSchema,
    LiveAuthoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CampaignStatus {
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "PARTIAL")]
    Partial,
    #[serde(rename = "FAIL_CLOSED")]
    FailClosed,
}

impl CampaignStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Partial => "PARTIAL",
            Self::FailClosed => "FAIL_CLOSED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ExpectedCell {
    pub profile: ProfileId,
    pub adapter: AdapterId,
    pub outcome_class: OutcomeClass,
    pub task_success: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("schema: {0}")]
    Schema(String),
    #[error("policy: {0}")]
    Policy(String),
    #[error("host: {0}")]
    Host(String),
    #[error("verifier: {0}")]
    Verifier(String),
    #[error("io: {0}")]
    Io(String),
}

pub type EvalResult<T> = Result<T, EvalError>;

pub fn validate_id(name: &str, value: &str) -> EvalResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(EvalError::Schema(format!("invalid {name}")));
    }
    Ok(())
}
