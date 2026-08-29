//! Operator-authored task objectives and their closed success predicates.
//!
//! A verified action receipt proves that *one low-level action ran and had a
//! visible effect on the next frame*. That is not the same claim as "the thing
//! the operator asked for is done", and treating it as such lets a model
//! terminate a run successfully after a single unrelated keystroke.
//!
//! A [`ComputerTaskSpec`] closes that gap. The operator states the objective
//! and, with it, a **closed** predicate over observable frame state that must
//! hold for the run to be called successful. The predicate is authored by the
//! host or the operator, never by the model, and its grammar is a fixed enum:
//! there is no expression language, no model-supplied matcher, and nothing a
//! proposal can add to it.
//!
//! The objective text is bound by digest, so a spec cannot be paired with a
//! different objective than the one the model was actually given.
//!
//! Locators are deliberately **not** element IDs. Semantic element IDs are
//! documented as ephemeral per observation, so an ID-based predicate would
//! silently stop matching after any re-observation — and a predicate that
//! cannot find its subject must never be read as success. Locators address an
//! element by its stable role and label instead, and a locator that matches
//! nothing is an explicit failure, never a pass.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::types::{
    validate_text, ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult,
    SemanticElement, MAX_LABEL_BYTES,
};

/// Wire version for the durable task spec.
pub const TASK_SPEC_VERSION: u32 = 1;

/// Upper bound on predicate breadth, so a spec cannot be used to smuggle an
/// unbounded evaluation cost or an unbounded durable record.
const MAX_PREDICATE_CLAUSES: usize = 16;
const MAX_OBJECTIVE_BYTES: usize = 4 * 1024;

/// Frame-stable address for one observed element.
///
/// Role is required and label is optional but, when present, must match
/// exactly. Neither is an element ID: the point of this type is to survive
/// re-observation, which an ephemeral ID does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementLocator {
    pub role: String,
    pub label: Option<String>,
}

impl ElementLocator {
    pub fn new(role: impl Into<String>, label: Option<String>) -> Self {
        Self {
            role: role.into(),
            label,
        }
    }

    fn validate(&self) -> ComputerResult<()> {
        validate_text("role", &self.role, 128)?;
        if let Some(label) = &self.label {
            validate_text("label", label, MAX_LABEL_BYTES)?;
        }
        Ok(())
    }

    /// Resolve on one frame. `None` when nothing matches, and also when the
    /// address is ambiguous: two candidates mean the predicate does not name a
    /// single subject, which is a failure rather than a coin flip.
    pub fn resolve<'a>(&self, observation: &'a ComputerObservation) -> Option<&'a SemanticElement> {
        let mut matches = observation.elements.iter().filter(|element| {
            element.role == self.role
                && match &self.label {
                    Some(label) => element.label.as_deref() == Some(label.as_str()),
                    None => true,
                }
        });
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    fn digest_into(&self, hasher: &mut Sha256) {
        hasher.update(self.role.as_bytes());
        hasher.update([0]);
        hasher.update(self.label.as_deref().unwrap_or("").as_bytes());
        hasher.update([0]);
    }
}

/// Closed grammar for "the objective is visibly satisfied".
///
/// Every variant is decided against one observation. There is no negation of
/// an unresolvable locator that could pass by accident: `ElementAbsent`
/// requires the frame to have been observed and the locator to resolve to
/// nothing, which is a positive statement about a frame the host captured, not
/// an inference from missing evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskPredicate {
    /// The addressed element exists and carries exactly this value.
    ElementValueEquals {
        locator: ElementLocator,
        value: String,
    },
    /// The addressed element exists and is enabled.
    ElementEnabled { locator: ElementLocator },
    /// The addressed element exists and reports itself focused.
    ElementFocused { locator: ElementLocator },
    /// The addressed element is not present on the frame.
    ElementAbsent { locator: ElementLocator },
    /// Every clause holds. An empty `All` is rejected at validation: a
    /// vacuously true objective is exactly the failure mode this type exists
    /// to prevent.
    All { clauses: Vec<TaskPredicate> },
}

impl TaskPredicate {
    fn validate(&self, depth: usize) -> ComputerResult<()> {
        if depth > 2 {
            return Err(invalid("task predicate is nested too deeply"));
        }
        match self {
            Self::ElementValueEquals { locator, value } => {
                locator.validate()?;
                validate_text("value", value, MAX_LABEL_BYTES)
            }
            Self::ElementEnabled { locator }
            | Self::ElementFocused { locator }
            | Self::ElementAbsent { locator } => locator.validate(),
            Self::All { clauses } => {
                if clauses.is_empty() || clauses.len() > MAX_PREDICATE_CLAUSES {
                    return Err(invalid(
                        "task predicate must have between one and 16 clauses",
                    ));
                }
                for clause in clauses {
                    clause.validate(depth + 1)?;
                }
                Ok(())
            }
        }
    }

    /// Decide the predicate against one frame.
    ///
    /// Returns the first unmet clause's reason so an operator sees *why* a run
    /// stopped for review rather than completing. The reason names the locator,
    /// which is operator-authored, never observed content.
    pub fn evaluate(&self, observation: &ComputerObservation) -> Result<(), String> {
        match self {
            Self::ElementValueEquals { locator, value } => {
                let element = locator
                    .resolve(observation)
                    .ok_or_else(|| unresolved(locator))?;
                if element.value.as_deref() == Some(value.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "{} does not carry the expected value",
                        describe(locator)
                    ))
                }
            }
            Self::ElementEnabled { locator } => {
                let element = locator
                    .resolve(observation)
                    .ok_or_else(|| unresolved(locator))?;
                element
                    .enabled
                    .then_some(())
                    .ok_or_else(|| format!("{} is not enabled", describe(locator)))
            }
            Self::ElementFocused { locator } => {
                let element = locator
                    .resolve(observation)
                    .ok_or_else(|| unresolved(locator))?;
                element
                    .focused
                    .then_some(())
                    .ok_or_else(|| format!("{} is not focused", describe(locator)))
            }
            Self::ElementAbsent { locator } => match locator.resolve(observation) {
                Some(_) => Err(format!("{} is still present", describe(locator))),
                None => Ok(()),
            },
            Self::All { clauses } => {
                for clause in clauses {
                    clause.evaluate(observation)?;
                }
                Ok(())
            }
        }
    }

    fn digest_into(&self, hasher: &mut Sha256) {
        match self {
            Self::ElementValueEquals { locator, value } => {
                hasher.update(b"value_equals");
                locator.digest_into(hasher);
                hasher.update(value.as_bytes());
            }
            Self::ElementEnabled { locator } => {
                hasher.update(b"enabled");
                locator.digest_into(hasher);
            }
            Self::ElementFocused { locator } => {
                hasher.update(b"focused");
                locator.digest_into(hasher);
            }
            Self::ElementAbsent { locator } => {
                hasher.update(b"absent");
                locator.digest_into(hasher);
            }
            Self::All { clauses } => {
                hasher.update(b"all");
                hasher.update((clauses.len() as u64).to_be_bytes());
                for clause in clauses {
                    clause.digest_into(hasher);
                }
            }
        }
        hasher.update([0]);
    }
}

/// One operator-authored task: what was asked for, and what proves it done.
///
/// `objective_digest` binds the spec to the exact objective text the model was
/// given. A model turn carried out against a different objective cannot be
/// completed against this spec, because the digest the proposal path recomputes
/// will not match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTaskSpec {
    pub spec_version: u32,
    /// SHA-256 of the canonical objective text. The text itself is not stored:
    /// it is operator-authored prose that has no business in a durable record
    /// or a projection.
    pub objective_digest: String,
    pub predicate: TaskPredicate,
    /// Actions the operator authorizes for this objective. Every one of them
    /// still requires its own explicit approval; this only bounds how many
    /// approvals the objective may consume.
    pub max_actions: u32,
}

impl ComputerTaskSpec {
    /// Author a spec for an exact objective. Host/operator entry point.
    pub fn new(
        objective: &str,
        predicate: TaskPredicate,
        max_actions: u32,
    ) -> ComputerResult<Self> {
        let objective = objective.trim();
        if objective.is_empty() || objective.len() > MAX_OBJECTIVE_BYTES {
            return Err(invalid("task objective is empty or oversized"));
        }
        if max_actions == 0 {
            return Err(invalid("task spec must authorize at least one action"));
        }
        predicate.validate(0)?;
        Ok(Self {
            spec_version: TASK_SPEC_VERSION,
            objective_digest: objective_digest(objective),
            predicate,
            max_actions,
        })
    }

    /// Does this spec govern the objective the model was actually given?
    pub fn governs(&self, objective: &str) -> bool {
        self.spec_version == TASK_SPEC_VERSION
            && self.objective_digest == objective_digest(objective.trim())
    }

    /// Decide the objective against the current frame.
    pub fn evaluate(&self, observation: &ComputerObservation) -> Result<(), String> {
        if self.spec_version != TASK_SPEC_VERSION {
            return Err("task spec version is not current".into());
        }
        self.predicate.evaluate(observation)
    }

    /// Stable, content-free identity for the whole spec, folded into the
    /// authority binding so a swapped predicate invalidates every live seal.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"grokptah.computer.task_spec.v1");
        hasher.update([0]);
        hasher.update(self.spec_version.to_be_bytes());
        hasher.update(self.objective_digest.as_bytes());
        hasher.update(self.max_actions.to_be_bytes());
        self.predicate.digest_into(&mut hasher);
        format!("{:x}", hasher.finalize())
    }
}

/// SHA-256 of the exact objective text, so the text never has to be stored.
pub fn objective_digest(objective: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.computer.objective.v1");
    hasher.update([0]);
    hasher.update(objective.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn describe(locator: &ElementLocator) -> String {
    match &locator.label {
        Some(label) => format!("the {} labelled {label:?}", locator.role),
        None => format!("the {}", locator.role),
    }
}

fn unresolved(locator: &ElementLocator) -> String {
    format!(
        "{} is not uniquely present on the current observation",
        describe(locator)
    )
}

fn invalid(message: &str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;

    use super::*;
    use crate::computer_use::types::{
        ComputerTarget, ObservationGeometry, SemanticAction, Sensitivity,
    };

    fn element(role: &str, label: &str, value: Option<&str>, enabled: bool) -> SemanticElement {
        SemanticElement {
            element_id: format!("ephemeral-{}", Uuid7::next()),
            role: role.into(),
            label: Some(label.into()),
            value: value.map(Into::into),
            bounds: None,
            enabled,
            focused: false,
            sensitivity: Sensitivity::None,
            actions: BTreeSet::from([SemanticAction::SetValue]),
        }
    }

    /// Element IDs in these fixtures deliberately differ on every frame, which
    /// is what an ephemeral ID does in production.
    struct Uuid7;
    impl Uuid7 {
        fn next() -> String {
            uuid::Uuid::new_v4().to_string()
        }
    }

    fn frame(elements: Vec<SemanticElement>) -> ComputerObservation {
        ComputerObservation {
            observation_id: format!("observation-{}", Uuid7::next()),
            sequence: 1,
            target: ComputerTarget {
                app_id: "com.example.demo".into(),
                window_id: "window-1".into(),
                generation: 1,
                display_name: "Demo".into(),
                sensitivity: Sensitivity::None,
            },
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements,
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        }
    }

    fn name_locator() -> ElementLocator {
        ElementLocator::new("text_field", Some("Name".into()))
    }

    #[test]
    fn a_locator_survives_ephemeral_element_ids() {
        let first = frame(vec![element("text_field", "Name", Some("Ada"), true)]);
        let second = frame(vec![element("text_field", "Name", Some("Ada"), true)]);
        assert_ne!(
            first.elements[0].element_id, second.elements[0].element_id,
            "fixture must model ephemeral ids"
        );
        let predicate = TaskPredicate::ElementValueEquals {
            locator: name_locator(),
            value: "Ada".into(),
        };
        assert!(predicate.evaluate(&first).is_ok());
        assert!(predicate.evaluate(&second).is_ok());
    }

    /// The core P0 property: a locator that resolves to nothing is a failure,
    /// never a pass. Missing evidence is not success.
    #[test]
    fn an_unresolvable_locator_never_satisfies_a_predicate() {
        let empty = frame(Vec::new());
        for predicate in [
            TaskPredicate::ElementValueEquals {
                locator: name_locator(),
                value: "Ada".into(),
            },
            TaskPredicate::ElementEnabled {
                locator: name_locator(),
            },
            TaskPredicate::ElementFocused {
                locator: name_locator(),
            },
        ] {
            assert!(
                predicate.evaluate(&empty).is_err(),
                "an absent subject must not satisfy {predicate:?}"
            );
        }
    }

    /// An ambiguous locator names no single subject, so it fails rather than
    /// silently choosing one.
    #[test]
    fn an_ambiguous_locator_is_a_failure() {
        let ambiguous = frame(vec![
            element("text_field", "Name", Some("Ada"), true),
            element("text_field", "Name", Some("Grace"), true),
        ]);
        let predicate = TaskPredicate::ElementValueEquals {
            locator: name_locator(),
            value: "Ada".into(),
        };
        assert!(predicate.evaluate(&ambiguous).is_err());
    }

    #[test]
    fn a_wrong_value_or_disabled_subject_fails() {
        let wrong = frame(vec![element("text_field", "Name", Some("Grace"), true)]);
        assert!(TaskPredicate::ElementValueEquals {
            locator: name_locator(),
            value: "Ada".into(),
        }
        .evaluate(&wrong)
        .is_err());

        let disabled = frame(vec![element("text_field", "Name", Some("Ada"), false)]);
        assert!(TaskPredicate::ElementEnabled {
            locator: name_locator(),
        }
        .evaluate(&disabled)
        .is_err());
    }

    #[test]
    fn all_requires_every_clause() {
        let observed = frame(vec![
            element("text_field", "Name", Some("Ada"), true),
            element("button", "Submit", None, false),
        ]);
        let predicate = TaskPredicate::All {
            clauses: vec![
                TaskPredicate::ElementValueEquals {
                    locator: name_locator(),
                    value: "Ada".into(),
                },
                TaskPredicate::ElementEnabled {
                    locator: ElementLocator::new("button", Some("Submit".into())),
                },
            ],
        };
        assert!(predicate.evaluate(&observed).is_err(), "submit is disabled");
    }

    /// A spec cannot be vacuous, unbounded, or authored for zero actions.
    #[test]
    fn degenerate_specs_are_rejected() {
        let ok = TaskPredicate::ElementEnabled {
            locator: name_locator(),
        };
        assert!(ComputerTaskSpec::new("", ok.clone(), 1).is_err());
        assert!(ComputerTaskSpec::new("do the thing", ok.clone(), 0).is_err());
        assert!(
            ComputerTaskSpec::new("do the thing", TaskPredicate::All { clauses: vec![] }, 1)
                .is_err(),
            "a vacuously true objective must be rejected"
        );
        let too_many = TaskPredicate::All {
            clauses: (0..MAX_PREDICATE_CLAUSES + 1).map(|_| ok.clone()).collect(),
        };
        assert!(ComputerTaskSpec::new("do the thing", too_many, 1).is_err());
    }

    /// The spec is bound to its objective text, so it cannot be re-pointed at a
    /// different ask.
    #[test]
    fn a_spec_only_governs_its_own_objective() {
        let spec = ComputerTaskSpec::new(
            "Enter Ada Lovelace in the Name field",
            TaskPredicate::ElementValueEquals {
                locator: name_locator(),
                value: "Ada Lovelace".into(),
            },
            2,
        )
        .expect("spec");
        assert!(spec.governs("Enter Ada Lovelace in the Name field"));
        assert!(spec.governs("  Enter Ada Lovelace in the Name field  "));
        assert!(!spec.governs("Delete every file in the Documents folder"));
        assert!(!spec.objective_digest.contains("Ada"));
        assert_eq!(spec.objective_digest.len(), 64);
    }

    /// Changing any part of the predicate changes the digest, so a swapped
    /// predicate cannot ride an existing authority binding.
    #[test]
    fn spec_digests_cover_every_component() {
        let base = ComputerTaskSpec::new(
            "objective",
            TaskPredicate::ElementValueEquals {
                locator: name_locator(),
                value: "Ada".into(),
            },
            2,
        )
        .expect("spec");
        let other_value = ComputerTaskSpec::new(
            "objective",
            TaskPredicate::ElementValueEquals {
                locator: name_locator(),
                value: "Grace".into(),
            },
            2,
        )
        .expect("spec");
        let other_locator = ComputerTaskSpec::new(
            "objective",
            TaskPredicate::ElementValueEquals {
                locator: ElementLocator::new("text_field", Some("Email".into())),
                value: "Ada".into(),
            },
            2,
        )
        .expect("spec");
        let other_budget = ComputerTaskSpec::new(
            "objective",
            TaskPredicate::ElementValueEquals {
                locator: name_locator(),
                value: "Ada".into(),
            },
            3,
        )
        .expect("spec");
        let digests = [
            base.digest(),
            other_value.digest(),
            other_locator.digest(),
            other_budget.digest(),
        ];
        for (index, one) in digests.iter().enumerate() {
            for other in digests.iter().skip(index + 1) {
                assert_ne!(one, other);
            }
        }
    }
}
