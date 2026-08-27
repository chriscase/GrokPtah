//! Scripted model output.
//!
//! There is no provider call anywhere in this crate. A scenario carries a
//! `Plan`: the sequence a competent agent would follow if the surface behaved.
//! What the benchmark actually measures is what happens when it does not --
//! the plan is the constant, and the agent's handling of deviation is the
//! variable.
//!
//! Plan steps address elements **by label, never by id**. That is deliberate.
//! An id is valid for exactly one observation, so a plan written in ids would
//! be unrunnable the moment the tree is rebuilt; writing plans in labels
//! forces every agent to re-resolve against the current observation, which is
//! the behaviour the AX-reorder and stale-observation families are checking.

use serde::{Deserialize, Serialize};

use crate::schema::Key;

/// One step of a scripted plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanStep {
    /// Bring the target forward.
    Activate,
    InvokeLabel {
        label: String,
    },
    SetValueLabel {
        label: String,
        text: String,
    },
    SelectLabel {
        label: String,
    },
    /// Scroll until an element with this label is realized.
    ScrollToLabel {
        label: String,
    },
    PressKeys {
        keys: Vec<Key>,
    },
    /// Dismiss whatever modal owns input.
    DismissModal,
    /// Use a control inside the modal that owns input.
    ConfirmModal {
        label: String,
    },
    Wait {
        millis: u64,
    },
    /// Last-resort pointer click, in target-relative coordinates. Only
    /// reachable when the profile enables pointer fallback and no semantic
    /// path exists.
    PointerAt {
        x: i32,
        y: i32,
    },
    /// Claim the task is done. The oracle decides whether that is true.
    Finish,
}

impl PlanStep {
    /// The label this step is trying to reach, if any.
    #[must_use]
    pub fn target_label(&self) -> Option<&str> {
        match self {
            Self::InvokeLabel { label }
            | Self::SetValueLabel { label, .. }
            | Self::SelectLabel { label }
            | Self::ScrollToLabel { label }
            | Self::ConfirmModal { label } => Some(label.as_str()),
            _ => None,
        }
    }

    /// True when the step changes the world rather than just looking at it.
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::InvokeLabel { .. }
                | Self::SetValueLabel { .. }
                | Self::SelectLabel { .. }
                | Self::ConfirmModal { .. }
                | Self::PressKeys { .. }
                | Self::PointerAt { .. }
        )
    }
}

/// An ordered scripted plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub steps: Vec<PlanStep>,
}

impl Plan {
    #[must_use]
    pub fn new(steps: Vec<PlanStep>) -> Self {
        Self { steps }
    }

    #[must_use]
    pub fn step(&self, index: usize) -> Option<&PlanStep> {
        self.steps.get(index)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// A plan must end by claiming completion, or it can never succeed and
    /// can never be caught claiming falsely. The catalog gate asserts this.
    #[must_use]
    pub fn ends_with_finish(&self) -> bool {
        matches!(self.steps.last(), Some(PlanStep::Finish))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_steps_address_elements_by_label() {
        let step = PlanStep::InvokeLabel {
            label: "Save".into(),
        };
        assert_eq!(step.target_label(), Some("Save"));
        assert!(step.is_mutating());
    }

    #[test]
    fn scrolling_and_waiting_are_not_mutations() {
        assert!(
            !PlanStep::ScrollToLabel {
                label: "Row 40".into()
            }
            .is_mutating()
        );
        assert!(!PlanStep::Wait { millis: 100 }.is_mutating());
    }

    #[test]
    fn a_plan_without_a_finish_is_rejected() {
        assert!(!Plan::new(vec![PlanStep::Activate]).ends_with_finish());
        assert!(Plan::new(vec![PlanStep::Activate, PlanStep::Finish]).ends_with_finish());
    }
}
