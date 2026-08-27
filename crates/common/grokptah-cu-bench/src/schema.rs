//! Benchmark wire schema.
//!
//! These types deliberately mirror the vocabulary of
//! `grokptah-agent-bridge::computer_use` (`Sensitivity`, `ActionClass`,
//! `SemanticAction`, `ComputerErrorCode`, `ComputerControlDisposition`) so a
//! benchmark verdict reads in the same terms as a production audit entry.
//!
//! They are *not* re-exports. The bridge lives in a nested workspace that the
//! root workspace excludes, and the benchmark must be runnable as a
//! certification-lab artifact by someone who has the fixtures but not the
//! implementation. Keeping the schema standalone is what makes the harness a
//! qualification authority rather than a self-test: a candidate build is
//! judged against a written contract, not against its own types.
//!
//! The mapping is asserted in `tests/cu_bench_vocabulary.rs`, so drift between
//! this schema and the production enums fails CI rather than silently
//! producing a verdict about the wrong contract.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Schema version. Bumped when a change would invalidate a stored report.
pub const SCHEMA_VERSION: &str = "grokptah.cu-bench/1";

/// Privacy classification of a surface, element, or observation.
///
/// `Secure` and `SystemRestricted` are *hard denied*: they can never be
/// exposed to a model, at any profile, for any model class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    None,
    Potential,
    Secure,
    SystemRestricted,
}

impl Sensitivity {
    #[must_use]
    pub fn is_hard_denied(self) -> bool {
        matches!(self, Self::Secure | Self::SystemRestricted)
    }
}

/// Semantic affordance advertised by an accessibility node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAction {
    Invoke,
    SetValue,
    Select,
    Scroll,
}

/// Authorization class an action is charged against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Semantic,
    TextEntry,
    KeyChord,
    PointerFallback,
}

/// Refusal taxonomy. Mirrors `ComputerErrorCode` on the production side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    InvalidRequest,
    InvalidState,
    Unauthorized,
    PermissionRequired,
    PermissionDenied,
    PermissionRevoked,
    ForbiddenTarget,
    ForbiddenAction,
    SensitiveSurface,
    StaleObservation,
    TargetChanged,
    TargetClosed,
    LimitReached,
    Conflict,
    UncertainOutcome,
    Interrupted,
    BackendUnavailable,
}

/// Operator-control disposition layered on the run lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlDisposition {
    #[default]
    AgentOwned,
    Paused,
    OperatorTakeover,
    Stopped,
    Interrupted,
    UncertainOutcome,
}

impl ControlDisposition {
    /// True when the agent no longer owns the surface and must not act, even
    /// if it still holds a syntactically valid grant.
    #[must_use]
    pub fn agent_may_act(self) -> bool {
        matches!(self, Self::AgentOwned)
    }
}

/// Integer logical geometry.
///
/// Production geometry is `f64`; the benchmark restricts it to integers so
/// every digest is bit-reproducible across targets. See `digest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    #[must_use]
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// The surface a run is bound to.
///
/// `generation` is the anti-reuse counter: any change to app, window, or
/// generation invalidates every outstanding observation and grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceTarget {
    pub app_id: String,
    pub window_id: String,
    pub generation: u64,
    pub display_name: String,
    pub sensitivity: Sensitivity,
    /// Present only for browser-class surfaces. Origin is what matters for
    /// the app/URL/window mismatch family, so it is carried separately from
    /// the display name a model can be socially engineered by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_origin: Option<String>,
}

/// A screenshot, represented only as bounded digests and regions.
///
/// The benchmark never carries pixels. A model that needs to "look" gets a
/// per-region digest plus a coarse luminance bucket and an `ambiguous` flag.
/// That is enough to model the ambiguous-pixel family honestly (a vision
/// model can resolve regions a text-only model cannot) without ever putting
/// image bytes, and therefore any real screen content, into a fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenDigest {
    pub content_sha256: String,
    pub width: u32,
    pub height: u32,
    /// Always true. An unredacted screenshot is never exposed, so the field
    /// exists to be asserted on, not to be toggled.
    pub redacted: bool,
    pub regions: Vec<ScreenRegion>,
}

/// One bounded region of a screenshot digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRegion {
    pub region_id: String,
    pub bounds: Rect,
    pub content_sha256: String,
    /// 0..=7. Coarse enough that it cannot reconstruct content.
    pub luminance_bucket: u8,
    /// True when pixels alone cannot disambiguate this region -- the case a
    /// model must not resolve by guessing.
    pub ambiguous: bool,
}

/// One accessibility element as exposed to the model.
///
/// `element_id` is scoped to exactly one observation. Reusing an id across
/// observations is the stale-reference hazard and is refused by the guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedElement {
    pub element_id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub sensitivity: Sensitivity,
    pub actions: BTreeSet<SemanticAction>,
    /// True when the element is scrolled out of the realized viewport. A
    /// virtualized row that is not realized cannot be acted on.
    #[serde(default)]
    pub offscreen: bool,
}

/// A complete observation handed to the model at one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub observation_id: String,
    pub sequence: u64,
    pub target: SurfaceTarget,
    /// Virtual clock, milliseconds since run start. Never a wall clock.
    pub captured_at_millis: u64,
    pub viewport: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenDigest>,
    pub elements: Vec<ObservedElement>,
    pub elements_truncated: bool,
    pub sensitivity: Sensitivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<String>,
    pub control_disposition: ControlDisposition,
    /// Another controller holding this surface, if the coordinator knows of
    /// one. Reported in the observation rather than discovered by having an
    /// action refused: an agent should be able to stand down *before* it
    /// proposes something, and a benchmark that forces a bad proposal to
    /// reveal a conflict is measuring its own harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub competing_controller: Option<String>,
    /// Whether the surface currently has network. Offline is a legitimate
    /// state to wait out, not a failure.
    #[serde(default = "default_true")]
    pub online: bool,
}

fn default_true() -> bool {
    true
}

impl Observation {
    #[must_use]
    pub fn element(&self, element_id: &str) -> Option<&ObservedElement> {
        self.elements
            .iter()
            .find(|element| element.element_id == element_id)
    }

    /// Elements whose label matches exactly. More than one is the duplicated
    /// label hazard.
    #[must_use]
    pub fn by_label<'a>(&'a self, label: &str) -> Vec<&'a ObservedElement> {
        self.elements
            .iter()
            .filter(|element| element.label.as_deref() == Some(label))
            .collect()
    }
}

/// A key the benchmark can model. Deliberately a small closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
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

/// A concrete action against the surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SurfaceAction {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_id: Option<String>,
        delta_x: i32,
        delta_y: i32,
    },
    KeyChord {
        keys: Vec<Key>,
    },
    /// Target-relative logical coordinates. Never global screen coordinates.
    PointerClick {
        x: i32,
        y: i32,
    },
    Wait {
        millis: u64,
    },
}

impl SurfaceAction {
    #[must_use]
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

    #[must_use]
    pub fn referenced_element(&self) -> Option<&str> {
        match self {
            Self::Invoke { element_id }
            | Self::SetValue { element_id, .. }
            | Self::Select { element_id } => Some(element_id.as_str()),
            Self::Scroll { element_id, .. } => element_id.as_deref(),
            _ => None,
        }
    }

    /// True for actions whose effect on the world cannot be undone by the
    /// harness. Used by the false-success and unsafe-action scoring.
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        !matches!(self, Self::Wait { .. } | Self::Scroll { .. })
    }
}

/// What the model chose to do at one step.
///
/// The three non-action variants are first-class: a benchmark that can only
/// score actions cannot tell a correct refusal from a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelIntent {
    /// Drive the surface.
    Act { action: SurfaceAction },
    /// Stop and hand the decision to a human, with a stated reason.
    Escalate { reason: EscalationReason },
    /// Stop without acting and without asking, because the task should not
    /// be attempted from this state.
    Abstain { reason: AbstentionReason },
    /// Claim the task is complete. Checked against the world oracle.
    DeclareSuccess,
}

/// Why the model wants a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    AmbiguousTarget,
    DestructiveAction,
    CredentialSurface,
    UnexpectedNavigation,
    SurfaceMismatch,
    ConflictingController,
    RecoveryUnavailable,
    OutOfScopeInstruction,
    LimitReached,
}

/// Why the model declined to act at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionReason {
    /// The visible content tried to redirect the task.
    InjectedInstruction,
    /// The surface holds credentials or other hard-denied content.
    SensitiveSurface,
    /// Observation no longer describes the surface.
    StaleObservation,
    /// Pixels alone cannot resolve the choice and no semantic path exists.
    UnresolvablePixels,
    /// The agent no longer owns the surface.
    ControlLost,
    /// Repeating would make no progress.
    NoProgress,
}

/// Coarse confidence. Buckets, not floats, so replay is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// One scripted model turn. There is no provider call anywhere in this crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelTurn {
    pub intent: ModelIntent,
    pub confidence: Confidence,
    /// Modeled prompt tokens consumed by this turn.
    pub prompt_tokens: u32,
    /// Modeled completion tokens produced by this turn.
    pub completion_tokens: u32,
    /// Modeled think+generate latency, in milliseconds of virtual time.
    pub latency_millis: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_classes_match_the_production_split() {
        assert_eq!(SurfaceAction::ActivateTarget.class(), ActionClass::Semantic);
        assert_eq!(
            SurfaceAction::SetValue {
                element_id: "e0".into(),
                text: "x".into()
            }
            .class(),
            ActionClass::TextEntry
        );
        assert_eq!(
            SurfaceAction::KeyChord {
                keys: vec![Key::Enter]
            }
            .class(),
            ActionClass::KeyChord
        );
        assert_eq!(
            SurfaceAction::PointerClick { x: 1, y: 1 }.class(),
            ActionClass::PointerFallback
        );
    }

    #[test]
    fn hard_denied_matches_production_definition() {
        assert!(!Sensitivity::None.is_hard_denied());
        assert!(!Sensitivity::Potential.is_hard_denied());
        assert!(Sensitivity::Secure.is_hard_denied());
        assert!(Sensitivity::SystemRestricted.is_hard_denied());
    }

    #[test]
    fn only_agent_owned_may_act() {
        assert!(ControlDisposition::AgentOwned.agent_may_act());
        for disposition in [
            ControlDisposition::Paused,
            ControlDisposition::OperatorTakeover,
            ControlDisposition::Stopped,
            ControlDisposition::Interrupted,
            ControlDisposition::UncertainOutcome,
        ] {
            assert!(!disposition.agent_may_act(), "{disposition:?} must not act");
        }
    }
}
