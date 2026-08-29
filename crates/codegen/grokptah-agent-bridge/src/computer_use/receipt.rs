//! Host-issued action receipts: the only positive evidence that can authorize
//! a Computer Run completion (#456).
//!
//! A run's `last_outcome` records *that* a dispatch reported success. It does
//! not record *which frame* that success belongs to, so on its own it can
//! authorize a completion against an observation it never verified. An
//! [`ActionReceipt`] closes that gap by binding one dispatch to:
//!
//! - the exact frame it was authorized against (`dispatch_frame`),
//! - the exact accepted action (`action_fingerprint`),
//! - a host-minted receipt identity the model never sees or chooses,
//! - the authority revision in force at dispatch (`control_epoch`),
//! - the backend's reported outcome, and
//! - at most one host-issued *verifying* frame.
//!
//! The verifying frame is captured by the host immediately after dispatch, in
//! the same authority epoch, through
//! [`super::service::ComputerUseService::observe_postcondition`]. Only that one
//! frame can carry the proof. Any ordinary observation, any authority change,
//! and restart recovery clear the receipt outright, so a positive outcome can
//! never travel forward to a frame it did not verify.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::objective::ElementLocator;
use super::types::{
    ActionClass, ActionOutcome, ComputerAction, ComputerObservation, SemanticElement,
};

/// Wire version for the durable receipt record. Bumped whenever the fields the
/// completion proof depends on change meaning, so a record written by an older
/// build can never be read as a newer, stronger proof.
pub const ACTION_RECEIPT_VERSION: u32 = 1;

/// Exact identity of one observation frame.
///
/// Both halves are required: an ID alone cannot order two frames, and a
/// sequence alone cannot distinguish two runs' frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIdentity {
    pub observation_id: String,
    pub sequence: u64,
}

impl FrameIdentity {
    pub fn of(observation: &ComputerObservation) -> Self {
        Self {
            observation_id: observation.observation_id.clone(),
            sequence: observation.sequence,
        }
    }

    pub fn matches(&self, observation: &ComputerObservation) -> bool {
        self.observation_id == observation.observation_id && self.sequence == observation.sequence
    }
}

/// What the host can re-check about a dispatched action on a later frame.
///
/// Addressed by [`ElementLocator`], not by element ID. Semantic element IDs are
/// ephemeral per observation, so an ID-addressed expectation would stop
/// resolving after any re-observation — and an expectation that cannot find its
/// subject must never be read as met.
///
/// [`PostconditionExpectation::Opaque`] is the honest answer for an action
/// whose effect a semantic frame cannot show. It can never be met, so such an
/// action can never carry a completion proof. That is deliberate: the
/// alternative is to let "we could not check" stand in for "it worked".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PostconditionExpectation {
    /// The addressed element must resolve uniquely on the verifying frame and
    /// carry exactly this value.
    ElementValue {
        locator: ElementLocator,
        value: String,
    },
    /// Nothing about this action is checkable from a semantic frame. Never met.
    Opaque,
}

impl PostconditionExpectation {
    /// Derive from the action and the frame it was authorized against, so the
    /// locator is built from the element the operator actually approved.
    fn derive(action: &ComputerAction, dispatch_frame: &ComputerObservation) -> Self {
        let ComputerAction::SetValue { element_id, text } = action else {
            return Self::Opaque;
        };
        let Some(element) = dispatch_frame.element(element_id) else {
            return Self::Opaque;
        };
        let locator = ElementLocator::new(element.role.clone(), element.label.clone());
        // A locator that is already ambiguous on the dispatch frame cannot be
        // trusted to name one subject later either.
        if locator.resolve(dispatch_frame).is_none() {
            return Self::Opaque;
        }
        Self::ElementValue {
            locator,
            value: text.clone(),
        }
    }

    /// Strictly positive: `true` only when the expectation is checkable on this
    /// frame **and** holds. Opaque and unresolvable both answer `false`.
    fn holds_on(&self, observation: &ComputerObservation) -> bool {
        let Self::ElementValue { locator, value } = self else {
            return false;
        };
        locator
            .resolve(observation)
            .is_some_and(|element: &SemanticElement| {
                element.value.as_deref() == Some(value.as_str())
            })
    }
}

/// Whether a receipt has acquired its one host-issued verifying frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReceiptVerification {
    /// Dispatched. No frame has verified it yet, so it cannot complete a run.
    Pending,
    /// The host captured this exact frame as the postcondition frame for the
    /// dispatch, in the dispatch's own authority epoch.
    Verified {
        frame: FrameIdentity,
        verified_at: DateTime<Utc>,
    },
}

/// One dispatch, bound to the frame it was authorized against and to at most
/// one host-issued verifying frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionReceipt {
    pub receipt_version: u32,
    /// Host-minted at dispatch commit. Never chosen by a model, a caller, or a
    /// backend, so it cannot be predicted and replayed from outside the host.
    pub receipt_id: String,
    pub run_id: String,
    /// The frame the action was authorized and dispatched against.
    pub dispatch_frame: FrameIdentity,
    pub action_fingerprint: String,
    pub action_class: ActionClass,
    /// Authority revision in force at dispatch. A pause, takeover, stop, or
    /// recovery moves the epoch and strands the receipt.
    pub control_epoch: u64,
    pub dispatched_at: DateTime<Utc>,
    pub outcome: ActionOutcome,
    pub expectation: PostconditionExpectation,
    pub verification: ReceiptVerification,
}

impl ActionReceipt {
    /// Mint a receipt for a committed dispatch. `receipt_id` is supplied by the
    /// host mutation path, never by a backend or a caller.
    pub(super) fn mint(
        receipt_id: String,
        run_id: &str,
        dispatch_observation: &ComputerObservation,
        action: &ComputerAction,
        control_epoch: u64,
        outcome: ActionOutcome,
    ) -> Self {
        Self {
            receipt_version: ACTION_RECEIPT_VERSION,
            receipt_id,
            run_id: run_id.to_string(),
            dispatch_frame: FrameIdentity::of(dispatch_observation),
            action_fingerprint: action_fingerprint(run_id, action),
            action_class: action.class(),
            control_epoch,
            dispatched_at: Utc::now(),
            outcome,
            expectation: PostconditionExpectation::derive(action, dispatch_observation),
            verification: ReceiptVerification::Pending,
        }
    }

    /// The backend reported the action's expected postcondition met.
    pub fn positive(&self) -> bool {
        self.outcome.expected_postcondition_met == Some(true)
    }

    /// Attach the single host-issued verifying frame.
    ///
    /// Returns `false` — leaving the receipt untouched for the caller to drop —
    /// when the receipt has already been verified, when the authority epoch
    /// moved since dispatch, when the dispatch was not positive, when the
    /// candidate frame is not strictly newer than the dispatch frame, or when
    /// a checkable expectation fails on that frame.
    pub(super) fn verify_with(
        &mut self,
        observation: &ComputerObservation,
        control_epoch: u64,
    ) -> bool {
        if !matches!(self.verification, ReceiptVerification::Pending)
            || self.control_epoch != control_epoch
            || !self.positive()
            || self.receipt_version != ACTION_RECEIPT_VERSION
            || observation.sequence <= self.dispatch_frame.sequence
            || observation.observation_id == self.dispatch_frame.observation_id
            || !self.expectation.holds_on(observation)
        {
            return false;
        }
        self.verification = ReceiptVerification::Verified {
            frame: FrameIdentity::of(observation),
            verified_at: Utc::now(),
        };
        true
    }

    /// Does this receipt authorize completing the run against `observation`
    /// under `control_epoch`?
    ///
    /// Every clause is required. A receipt that is merely positive, merely
    /// current, or merely for the right action is not a completion proof.
    pub fn authorizes_completion(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        control_epoch: u64,
    ) -> bool {
        let ReceiptVerification::Verified { frame, .. } = &self.verification else {
            return false;
        };
        self.receipt_version == ACTION_RECEIPT_VERSION
            && self.run_id == run_id
            && self.control_epoch == control_epoch
            && self.positive()
            && frame.matches(observation)
            && self.expectation.holds_on(observation)
    }
}

/// The exact evidence identity a completion must present.
///
/// It is captured from the live run at normalization time and re-checked
/// against the live run at application time, so a proposal that was valid when
/// the model answered cannot apply after the frame, the receipt, or the
/// authority epoch has moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionProof {
    pub receipt_id: String,
    pub action_fingerprint: String,
    pub frame: FrameIdentity,
    pub control_epoch: u64,
}

impl CompletionProof {
    /// Capture the evidence a run currently offers, or `None` when it offers
    /// none. `None` is the common case and always fails a completion closed.
    pub fn capture(
        receipt: &ActionReceipt,
        observation: &ComputerObservation,
        control_epoch: u64,
    ) -> Option<Self> {
        receipt
            .authorizes_completion(&receipt.run_id, observation, control_epoch)
            .then(|| Self {
                receipt_id: receipt.receipt_id.clone(),
                action_fingerprint: receipt.action_fingerprint.clone(),
                frame: FrameIdentity::of(observation),
                control_epoch,
            })
    }
}

/// Stable, secret-free identity for one accepted action within one run.
///
/// Run-scoped so a fingerprint captured from one run cannot be presented as
/// evidence in another, and hashed so no observed document content survives
/// into a durable record or a projection.
pub fn action_fingerprint(run_id: &str, action: &ComputerAction) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.computer.action.v1");
    hasher.update([0]);
    hasher.update(run_id.as_bytes());
    hasher.update([0]);
    // `ComputerAction` is an internally tagged enum of scalars, so its JSON
    // encoding is deterministic for a given value.
    match serde_json::to_vec(action) {
        Ok(bytes) => hasher.update(&bytes),
        // A non-serializable action cannot be fingerprinted; fold in a distinct
        // domain marker so it can never collide with a real action.
        Err(_) => hasher.update(b"unserializable"),
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::computer_use::types::{
        ComputerTarget, ObservationGeometry, SemanticAction, Sensitivity,
    };

    /// Element IDs differ on every frame, as a real accessibility adapter's do.
    /// Anything that verifies here by remembering an ID is verifying by
    /// accident.
    fn frame(id: &str, sequence: u64, value: Option<&str>) -> ComputerObservation {
        ComputerObservation {
            observation_id: id.into(),
            sequence,
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
                width: 10.0,
                height: 10.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: format!("ephemeral-{}", uuid::Uuid::new_v4()),
                role: "text_field".into(),
                label: Some("Name".into()),
                value: value.map(Into::into),
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        }
    }

    fn empty_frame(id: &str, sequence: u64) -> ComputerObservation {
        ComputerObservation {
            elements: Vec::new(),
            ..frame(id, sequence, None)
        }
    }

    fn set_value_on(observation: &ComputerObservation, text: &str) -> ComputerAction {
        ComputerAction::SetValue {
            element_id: observation.elements[0].element_id.clone(),
            text: text.into(),
        }
    }

    fn receipt(dispatch: &ComputerObservation) -> ActionReceipt {
        ActionReceipt::mint(
            "receipt-1".into(),
            "run-1",
            dispatch,
            &set_value_on(dispatch, "Ada"),
            3,
            ActionOutcome::bounded("set", Some(true)),
        )
    }

    #[test]
    fn pending_receipt_never_authorizes_completion() {
        let dispatch = frame("observation-1", 1, None);
        let receipt = receipt(&dispatch);
        assert!(!receipt.authorizes_completion("run-1", &dispatch, 3));
    }

    #[test]
    fn verified_receipt_authorizes_only_its_own_frame() {
        let dispatch = frame("observation-1", 1, None);
        let verifying = frame("observation-2", 2, Some("Ada"));
        let mut receipt = receipt(&dispatch);
        assert!(receipt.verify_with(&verifying, 3));
        assert!(receipt.authorizes_completion("run-1", &verifying, 3));

        assert!(!receipt.authorizes_completion(
            "run-1",
            &frame("observation-3", 3, Some("Ada")),
            3
        ));
        assert!(!receipt.authorizes_completion("run-1", &verifying, 4));
        assert!(!receipt.authorizes_completion("run-2", &verifying, 3));
    }

    #[test]
    fn verification_is_single_use_and_epoch_bound() {
        let dispatch = frame("observation-1", 1, None);
        let mut receipt = receipt(&dispatch);
        assert!(!receipt.verify_with(&frame("observation-2", 2, Some("Ada")), 9));
        assert!(receipt.verify_with(&frame("observation-2", 2, Some("Ada")), 3));
        assert!(!receipt.verify_with(&frame("observation-3", 3, Some("Ada")), 3));
    }

    #[test]
    fn negative_outcome_can_never_verify() {
        let dispatch = frame("observation-1", 1, None);
        let mut receipt = ActionReceipt::mint(
            "receipt-1".into(),
            "run-1",
            &dispatch,
            &set_value_on(&dispatch, "Ada"),
            0,
            ActionOutcome::bounded("no postcondition", None),
        );
        let verifying = frame("observation-2", 2, Some("Ada"));
        assert!(!receipt.verify_with(&verifying, 0));
        assert!(!receipt.authorizes_completion("run-1", &verifying, 0));
    }

    /// An action whose effect a semantic frame cannot show is `Opaque`, and an
    /// opaque expectation can never be met. "We could not check" must not stand
    /// in for "it worked".
    #[test]
    fn an_opaque_expectation_can_never_verify() {
        let dispatch = frame("observation-1", 1, None);
        let mut receipt = ActionReceipt::mint(
            "receipt-1".into(),
            "run-1",
            &dispatch,
            &ComputerAction::ActivateTarget,
            0,
            ActionOutcome::bounded("activated", Some(true)),
        );
        assert_eq!(receipt.expectation, PostconditionExpectation::Opaque);
        let verifying = frame("observation-2", 2, Some("Ada"));
        assert!(!receipt.verify_with(&verifying, 0));
        assert!(!receipt.authorizes_completion("run-1", &verifying, 0));
    }

    /// The locator addresses role and label, not the ephemeral element ID, so
    /// it still resolves on a later frame.
    #[test]
    fn expectations_survive_ephemeral_element_ids() {
        let dispatch = frame("observation-1", 1, None);
        let verifying = frame("observation-2", 2, Some("Ada"));
        assert_ne!(
            dispatch.elements[0].element_id, verifying.elements[0].element_id,
            "fixture must model ephemeral ids"
        );
        let mut receipt = receipt(&dispatch);
        assert!(receipt.verify_with(&verifying, 3));
    }

    /// A subject that has vanished from the verifying frame is a failure, never
    /// a pass: missing evidence is not success.
    #[test]
    fn a_vanished_subject_never_verifies() {
        let dispatch = frame("observation-1", 1, None);
        let mut receipt = receipt(&dispatch);
        assert!(!receipt.verify_with(&empty_frame("observation-2", 2), 3));
        assert!(!receipt.authorizes_completion("run-1", &empty_frame("observation-2", 2), 3));
    }

    #[test]
    fn a_wrong_value_on_the_verifying_frame_never_verifies() {
        let dispatch = frame("observation-1", 1, None);
        let mut receipt = receipt(&dispatch);
        assert!(!receipt.verify_with(&frame("observation-2", 2, Some("Grace")), 3));
        assert!(receipt.verify_with(&frame("observation-2", 2, Some("Ada")), 3));
    }

    /// A verified receipt is re-checked against the frame it names, so a value
    /// that changes back after verification stops authorizing completion.
    #[test]
    fn a_verified_receipt_is_rechecked_at_completion_time() {
        let dispatch = frame("observation-1", 1, None);
        let verifying = frame("observation-2", 2, Some("Ada"));
        let mut receipt = receipt(&dispatch);
        assert!(receipt.verify_with(&verifying, 3));

        // Same frame identity, subject no longer carrying the value.
        let tampered = ComputerObservation {
            elements: Vec::new(),
            ..verifying.clone()
        };
        assert!(!receipt.authorizes_completion("run-1", &tampered, 3));
    }

    #[test]
    fn nonmonotonic_and_repeated_frames_cannot_verify() {
        let dispatch = frame("observation-2", 2, None);
        let mut receipt = receipt(&dispatch);
        assert!(!receipt.verify_with(&frame("observation-1", 1, Some("Ada")), 3));
        assert!(!receipt.verify_with(&frame("observation-2", 2, Some("Ada")), 3));
    }

    #[test]
    fn fingerprints_are_run_scoped_action_specific_and_content_free() {
        let action = ComputerAction::SetValue {
            element_id: "name".into(),
            text: "Ada Lovelace".into(),
        };
        let one = action_fingerprint("run-1", &action);
        assert_eq!(one, action_fingerprint("run-1", &action));
        assert_ne!(one, action_fingerprint("run-2", &action));
        assert_ne!(
            one,
            action_fingerprint(
                "run-1",
                &ComputerAction::SetValue {
                    element_id: "name".into(),
                    text: "Grace Hopper".into(),
                },
            )
        );
        assert!(!one.contains("Ada"));
        assert_eq!(one.len(), 64);
    }
}
