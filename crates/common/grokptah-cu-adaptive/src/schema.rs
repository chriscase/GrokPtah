//! The deterministic plan and verdict schema.
//!
//! Everything a planner may say is a variant of [`StepIntent`], and every
//! struct in this module is `deny_unknown_fields`. A planner cannot invent an
//! action, attach an extra argument to one, or smuggle a field past a
//! validator that does not know about it: an unrecognized key is a parse
//! failure, not an ignored key. That is the difference between a schema and a
//! suggestion, and it is why the same plan bytes mean the same thing to a
//! small local model's output parser and to a strong hosted one's.
//!
//! Three properties are load-bearing:
//!
//! * **Closed.** [`StepIntent`] mirrors the production kernel's action set
//!   exactly. There is no `Other`, no free-form `command`, and no escape
//!   hatch, so the adaptive layer cannot express an action the kernel cannot
//!   authorize.
//! * **Bounded.** Every count, delta, and length has a limit checked by
//!   [`PlanEnvelope::validate`]. A plan that would exhaust a budget by being
//!   enormous is refused before it costs anything.
//! * **Content-free on the wire.** The objective travels as a digest, and
//!   typed text travels as a [`TextPayload`] whose literal is never
//!   serialized. A plan is fully auditable and carries nothing readable out.

use serde::{Deserialize, Serialize};

use crate::confidence::{AmbiguityAssessment, Reversibility};
use crate::digest::{digest_canonical, domain, is_digest};
use crate::grounding::GroundingClaim;
use crate::horizon::Horizon;
use crate::lease::FrameToken;
use crate::profile::ProfileId;
use crate::redaction::TextPayload;
use crate::tier::ModelTier;
use crate::vocabulary::DenyReason;

/// Wire version of the plan/verdict contract. An executor refuses a plan that
/// does not match rather than guessing which fields it understands.
pub const ADAPTIVE_SCHEMA_VERSION: u16 = 1;

/// The largest scroll delta a single step may request, matching the kernel's
/// per-action bound.
pub const MAX_SCROLL_DELTA: i32 = 10_000;

/// The largest number of keys in one chord.
pub const MAX_CHORD_KEYS: usize = 4;

/// The longest a step may ask to wait.
pub const MAX_WAIT_MILLIS: u64 = 5_000;

/// A reference to one element in one frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElementRef {
    /// Ephemeral, frame-scoped identity. Never an OS handle.
    pub element_id: String,
    /// Bumped when the application recycles the identity.
    pub generation: u64,
}

impl ElementRef {
    pub fn new(element_id: impl Into<String>, generation: u64) -> Result<Self, DenyReason> {
        let candidate = Self {
            element_id: element_id.into(),
            generation,
        };
        if candidate.is_well_formed() {
            Ok(candidate)
        } else {
            Err(DenyReason::SchemaViolation)
        }
    }

    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.element_id.is_empty()
            && self.element_id.len() <= 256
            && self
                .element_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            && !self.element_id.contains("..")
    }
}

/// Keys a chord may use. Closed, and deliberately without printable
/// characters: text goes through [`StepIntent::SetValue`] where it is
/// redactable, not through a chord where it would not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChordKey {
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

/// Which pointer button a fallback step uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Primary,
    Secondary,
}

/// Broad families of intent, used for grounding requirements and grant
/// classes. This is the same partition the kernel's action classes use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentFamily {
    /// Names no element and mutates nothing: wait, observe, complete,
    /// activate the already authorized target.
    Ambient,
    /// Names an element and uses an advertised semantic action.
    Semantic,
    /// Types into a named element.
    TextEntry,
    /// Sends a key chord to the target.
    KeyChord,
    /// Leaves the semantic surface for a coordinate.
    PointerFallback,
}

impl IntentFamily {
    pub const ALL: &'static [IntentFamily] = &[
        Self::Ambient,
        Self::Semantic,
        Self::TextEntry,
        Self::KeyChord,
        Self::PointerFallback,
    ];

    /// True when a step in this family changes the world.
    #[must_use]
    pub fn mutates(self) -> bool {
        !matches!(self, Self::Ambient)
    }
}

/// One thing a planner may propose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepIntent {
    /// Bring the already authorized target forward. Names no new target.
    ActivateTarget,
    /// Look again.
    Observe,
    /// Wait for the application to settle.
    Wait { millis: u64 },
    /// Report the objective satisfied.
    Complete,
    /// Use an element's advertised primary action.
    Invoke { element: ElementRef },
    /// Choose an element.
    Select { element: ElementRef },
    /// Scroll, optionally within a named element.
    Scroll {
        #[serde(default)]
        element: Option<ElementRef>,
        delta_x: i32,
        delta_y: i32,
    },
    /// Type into a named element.
    SetValue {
        element: ElementRef,
        text: TextPayload,
    },
    /// Send a chord to the target.
    KeyChord { keys: Vec<ChordKey> },
    /// Click a coordinate inside a grounded region.
    PointerFallback {
        /// Target-relative logical coordinates, never global screen ones, and
        /// integers so a trace is byte-reproducible.
        x: i32,
        y: i32,
        button: PointerButton,
    },
}

impl StepIntent {
    #[must_use]
    pub fn family(&self) -> IntentFamily {
        match self {
            Self::ActivateTarget | Self::Observe | Self::Wait { .. } | Self::Complete => {
                IntentFamily::Ambient
            }
            Self::Invoke { .. } | Self::Select { .. } | Self::Scroll { .. } => {
                IntentFamily::Semantic
            }
            Self::SetValue { .. } => IntentFamily::TextEntry,
            Self::KeyChord { .. } => IntentFamily::KeyChord,
            Self::PointerFallback { .. } => IntentFamily::PointerFallback,
        }
    }

    /// The element this step names, if any.
    #[must_use]
    pub fn element(&self) -> Option<&ElementRef> {
        match self {
            Self::Invoke { element }
            | Self::Select { element }
            | Self::SetValue { element, .. } => Some(element),
            Self::Scroll { element, .. } => element.as_ref(),
            _ => None,
        }
    }

    /// The production kernel action tag this intent dispatches as, or `None`
    /// for the two intents that are control-plane rather than actions.
    ///
    /// `Observe` is an observation request and `Complete` is the planner
    /// saying it is done; neither is something the kernel executes. Everything
    /// else has to name an action the kernel already has, which is what keeps
    /// the adaptive layer from being able to express something the safety
    /// kernel cannot authorize. The mapping is asserted against the kernel's
    /// own action set by the bridge-side conformance test.
    #[must_use]
    pub fn kernel_action_tag(&self) -> Option<&'static str> {
        match self {
            Self::Observe | Self::Complete => None,
            Self::ActivateTarget => Some("activate_target"),
            Self::Wait { .. } => Some("wait"),
            Self::Invoke { .. } => Some("invoke"),
            Self::Select { .. } => Some("select"),
            Self::Scroll { .. } => Some("scroll"),
            Self::SetValue { .. } => Some("set_value"),
            Self::KeyChord { .. } => Some("key_chord"),
            Self::PointerFallback { .. } => Some("pointer_click"),
        }
    }

    /// Per-step bounds. Checked before a step costs anything.
    pub fn validate(&self) -> Result<(), DenyReason> {
        if self
            .element()
            .is_some_and(|element| !element.is_well_formed())
        {
            return Err(DenyReason::SchemaViolation);
        }
        match self {
            Self::Wait { millis } if *millis == 0 || *millis > MAX_WAIT_MILLIS => {
                Err(DenyReason::SchemaViolation)
            }
            Self::Scroll {
                delta_x, delta_y, ..
            } if delta_x.unsigned_abs() > MAX_SCROLL_DELTA.unsigned_abs()
                || delta_y.unsigned_abs() > MAX_SCROLL_DELTA.unsigned_abs()
                || (*delta_x == 0 && *delta_y == 0) =>
            {
                Err(DenyReason::SchemaViolation)
            }
            Self::KeyChord { keys } if keys.is_empty() || keys.len() > MAX_CHORD_KEYS => {
                Err(DenyReason::SchemaViolation)
            }
            Self::SetValue { text, .. } if !text.is_well_formed() => {
                Err(DenyReason::SchemaViolation)
            }
            Self::PointerFallback { x, y, .. } if *x < 0 || *y < 0 => {
                Err(DenyReason::SchemaViolation)
            }
            _ => Ok(()),
        }
    }
}

/// What the planner expects to be true after the step.
///
/// A postcondition is not free: checking one costs an observation. It is
/// carried anyway because a step whose success cannot be stated is a step
/// whose failure cannot be detected, and a run that cannot detect failure
/// retries forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "expect", rename_all = "snake_case", deny_unknown_fields)]
pub enum Postcondition {
    /// Nothing checkable; only legal for ambient steps.
    None,
    /// The named element becomes focused.
    ElementFocused { element: ElementRef },
    /// The named element's value digest matches.
    ElementValueDigest { element: ElementRef, digest: String },
    /// The named element disappears from the frame.
    ElementGone { element: ElementRef },
    /// The frame digest changes at all.
    FrameChanged,
}

impl Postcondition {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::None | Self::FrameChanged => true,
            Self::ElementFocused { element } | Self::ElementGone { element } => {
                element.is_well_formed()
            }
            Self::ElementValueDigest { element, digest } => {
                element.is_well_formed() && is_digest(digest)
            }
        }
    }
}

/// One planned step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedStep {
    pub index: u32,
    pub intent: StepIntent,
    pub grounding: GroundingClaim,
    pub ambiguity: AmbiguityAssessment,
    pub reversibility: Reversibility,
    pub expected: Postcondition,
}

impl PlannedStep {
    pub fn validate(&self) -> Result<(), DenyReason> {
        self.intent.validate()?;
        if !self.ambiguity.is_well_formed()
            || !self.grounding.is_well_formed()
            || !self.expected.is_well_formed()
        {
            return Err(DenyReason::SchemaViolation);
        }
        // The grounding claim and the intent must name the same element, or
        // the claim is grounding something the step will not touch.
        match (self.intent.element(), self.grounding.element()) {
            (Some(intended), Some(grounded)) if intended == grounded => {}
            (None, None) => {}
            // A pointer step names no element but must still be grounded in
            // the region an element occupies.
            (None, Some(_)) if self.intent.family() == IntentFamily::PointerFallback => {}
            _ => return Err(DenyReason::SchemaViolation),
        }
        if !self.intent.family().mutates() && self.expected != Postcondition::None {
            return Err(DenyReason::SchemaViolation);
        }
        Ok(())
    }
}

/// A bounded batch of steps decided against one frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanEnvelope {
    pub schema_version: u16,
    pub plan_id: String,
    /// The objective as a digest. The objective text itself never enters the
    /// plan, so it can never leave in one.
    pub objective_digest: String,
    pub frame: FrameToken,
    pub profile: ProfileId,
    pub tier: ModelTier,
    pub horizon: Horizon,
    pub steps: Vec<PlannedStep>,
}

impl PlanEnvelope {
    /// Validate the whole plan against the schema and the tier's declared plan
    /// depth.
    pub fn validate(&self) -> Result<(), DenyReason> {
        if self.schema_version != ADAPTIVE_SCHEMA_VERSION
            || self.plan_id.is_empty()
            || self.plan_id.len() > 128
            || !is_digest(&self.objective_digest)
            || !self.frame.is_well_formed()
            || self.steps.is_empty()
        {
            return Err(DenyReason::SchemaViolation);
        }
        let declared = self.tier.declared();
        if self.steps.len() > declared.max_plan_depth as usize {
            // Proposing a plan deeper than the class declared it can hold is a
            // capability gap, not a malformed plan: the fix is a stronger
            // model, so it is reported as one.
            return Err(DenyReason::EscalationRequired);
        }
        for (position, step) in self.steps.iter().enumerate() {
            if step.index as usize != position {
                return Err(DenyReason::SchemaViolation);
            }
            step.validate()?;
        }
        Ok(())
    }

    /// Canonical digest of the plan, used to bind a verdict to the exact plan
    /// it judged.
    #[must_use]
    pub fn digest(&self) -> Option<String> {
        digest_canonical(domain::PLAN, self)
    }
}

/// How a step's postcondition came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostconditionOutcome {
    /// Checked and satisfied.
    Met,
    /// Checked and not satisfied.
    Missed,
    /// Not checked, because the profile does not verify postconditions.
    NotChecked,
    /// The step did not run.
    NotApplicable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::AmbiguityAssessment;
    use crate::digest::{digest_str, domain};
    use crate::redaction::TextClass;

    fn element() -> ElementRef {
        ElementRef::new("field-1", 1).unwrap()
    }

    fn frame() -> FrameToken {
        FrameToken {
            frame_id: "frame-1".into(),
            sequence: 1,
            epoch: 0,
            captured_at_millis: 0,
            digest: digest_str(domain::FRAME, "frame-1"),
        }
    }

    fn step(intent: StepIntent) -> PlannedStep {
        let grounding = match intent.element() {
            Some(element) => GroundingClaim::Semantic {
                element: element.clone(),
                role_digest: digest_str(domain::ELEMENT_ROLE, "text_field"),
            },
            None => GroundingClaim::None,
        };
        let expected = if intent.family().mutates() {
            Postcondition::FrameChanged
        } else {
            Postcondition::None
        };
        PlannedStep {
            index: 0,
            intent,
            grounding,
            ambiguity: AmbiguityAssessment::unambiguous(9_000),
            reversibility: Reversibility::Reversible,
            expected,
        }
    }

    fn plan(steps: Vec<PlannedStep>) -> PlanEnvelope {
        PlanEnvelope {
            schema_version: ADAPTIVE_SCHEMA_VERSION,
            plan_id: "plan-1".into(),
            objective_digest: digest_str(domain::OBJECTIVE, "rename the row"),
            frame: frame(),
            profile: ProfileId::Balanced,
            tier: ModelTier::StrongHosted,
            horizon: Horizon::Short,
            steps,
        }
    }

    #[test]
    fn every_dispatchable_intent_names_a_kernel_action() {
        let text = TextPayload::new("value", TextClass::Benign).unwrap();
        let dispatchable = [
            StepIntent::ActivateTarget,
            StepIntent::Wait { millis: 10 },
            StepIntent::Invoke { element: element() },
            StepIntent::Select { element: element() },
            StepIntent::Scroll {
                element: None,
                delta_x: 1,
                delta_y: 0,
            },
            StepIntent::SetValue {
                element: element(),
                text,
            },
            StepIntent::KeyChord {
                keys: vec![ChordKey::Enter],
            },
            StepIntent::PointerFallback {
                x: 1,
                y: 1,
                button: PointerButton::Primary,
            },
        ];
        for intent in dispatchable {
            assert!(
                intent.kernel_action_tag().is_some(),
                "{intent:?} dispatches without naming a kernel action"
            );
        }
        assert!(StepIntent::Observe.kernel_action_tag().is_none());
        assert!(StepIntent::Complete.kernel_action_tag().is_none());
    }

    #[test]
    fn unknown_keys_are_refused_rather_than_ignored() {
        let json = serde_json::json!({
            "intent": "invoke",
            "element": {"elementId": "field-1", "generation": 1},
            "shell": "whoami"
        });
        let parsed: Result<StepIntent, _> = serde_json::from_value(json);
        assert!(parsed.is_err(), "an extra key was silently accepted");
    }

    #[test]
    fn there_is_no_escape_hatch_intent() {
        let json = serde_json::json!({"intent": "run_command", "argv": ["sh"]});
        let parsed: Result<StepIntent, _> = serde_json::from_value(json);
        assert!(parsed.is_err());
    }

    #[test]
    fn per_step_bounds_are_enforced() {
        assert_eq!(
            StepIntent::Wait {
                millis: MAX_WAIT_MILLIS + 1
            }
            .validate()
            .unwrap_err(),
            DenyReason::SchemaViolation
        );
        assert_eq!(
            StepIntent::KeyChord { keys: vec![] }
                .validate()
                .unwrap_err(),
            DenyReason::SchemaViolation
        );
        assert_eq!(
            StepIntent::KeyChord {
                keys: vec![ChordKey::Meta; MAX_CHORD_KEYS + 1]
            }
            .validate()
            .unwrap_err(),
            DenyReason::SchemaViolation
        );
        assert_eq!(
            StepIntent::Scroll {
                element: None,
                delta_x: 0,
                delta_y: MAX_SCROLL_DELTA + 1,
            }
            .validate()
            .unwrap_err(),
            DenyReason::SchemaViolation
        );
        assert_eq!(
            StepIntent::PointerFallback {
                x: -1,
                y: 0,
                button: PointerButton::Primary
            }
            .validate()
            .unwrap_err(),
            DenyReason::SchemaViolation
        );
    }

    #[test]
    fn grounding_must_name_the_element_the_step_touches() {
        let mut planned = step(StepIntent::Invoke { element: element() });
        planned.grounding = GroundingClaim::Semantic {
            element: ElementRef::new("other-element", 1).unwrap(),
            role_digest: digest_str(domain::ELEMENT_ROLE, "button"),
        };
        assert_eq!(planned.validate().unwrap_err(), DenyReason::SchemaViolation);
    }

    #[test]
    fn an_ambient_step_cannot_claim_a_postcondition() {
        let mut planned = step(StepIntent::Observe);
        planned.expected = Postcondition::FrameChanged;
        assert_eq!(planned.validate().unwrap_err(), DenyReason::SchemaViolation);
    }

    #[test]
    fn a_plan_deeper_than_the_class_declared_asks_for_a_stronger_one() {
        let depth = ModelTier::SmallLocal.declared().max_plan_depth as usize + 1;
        let mut steps = Vec::new();
        for index in 0..depth {
            let mut planned = step(StepIntent::Observe);
            planned.index = index as u32;
            steps.push(planned);
        }
        let envelope = PlanEnvelope {
            tier: ModelTier::SmallLocal,
            ..plan(steps)
        };
        assert_eq!(
            envelope.validate().unwrap_err(),
            DenyReason::EscalationRequired
        );
    }

    #[test]
    fn step_indices_must_match_their_position() {
        let mut first = step(StepIntent::Observe);
        first.index = 7;
        assert_eq!(
            plan(vec![first]).validate().unwrap_err(),
            DenyReason::SchemaViolation
        );
    }

    #[test]
    fn a_plan_carries_no_objective_text_and_no_typed_text() {
        let text = TextPayload::new("Ada Lovelace", TextClass::Benign).unwrap();
        let planned = step(StepIntent::SetValue {
            element: element(),
            text,
        });
        let envelope = plan(vec![planned]);
        envelope.validate().unwrap();
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(crate::redaction::leak_scan(&json, &["Ada Lovelace", "rename the row"]).is_empty());
    }

    #[test]
    fn plan_digests_are_stable_and_bind_every_field() {
        let envelope = plan(vec![step(StepIntent::Observe)]);
        let first = envelope.digest().unwrap();
        assert_eq!(
            first,
            plan(vec![step(StepIntent::Observe)]).digest().unwrap()
        );
        let changed = PlanEnvelope {
            profile: ProfileId::HighAssurance,
            ..plan(vec![step(StepIntent::Observe)])
        };
        assert_ne!(first, changed.digest().unwrap());
    }
}
