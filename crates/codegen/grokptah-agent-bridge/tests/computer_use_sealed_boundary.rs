//! Adversarial coverage for the sealed model-output boundary (#457) and the
//! current-frame completion proof (#456).
//!
//! Everything here runs against a deterministic in-process backend. No
//! provider is called, no OS input is dispatched, and no real application is
//! opened. Each test states the attack it defeats, because a green assertion
//! on its own does not say which bypass is closed.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use tempfile::TempDir;
use uuid::Uuid;

use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities,
    ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult, ComputerRun,
    ComputerRunState, ComputerStore, ComputerTarget, ComputerUseLimits, GrantIssuer,
    ObservationGeometry, SemanticAction, SemanticElement, Sensitivity,
};
use grokptah_agent_bridge::{
    accept_model_proposal, AcceptedIntent, ComputerUseService, ModelProposalContext,
    RawModelProposal,
};

const NAME_ELEMENT: &str = "name-field";
const DISABLED_ELEMENT: &str = "disabled-field";
const SECURE_ELEMENT: &str = "secure-field";

/// Deterministic backend whose observations are stable across frames, so a
/// test can isolate frame identity from element churn.
#[derive(Debug, Default)]
struct FixtureBackend {
    sequence: AtomicU64,
    value: parking_lot::Mutex<Option<String>>,
    positive: bool,
    /// Emit an element the policy hard-denies. Used to prove the kernel
    /// refuses to *expose* such a frame at all, so a model never sees one.
    secure: bool,
}

impl FixtureBackend {
    fn new(positive: bool) -> Self {
        Self {
            sequence: AtomicU64::new(0),
            value: parking_lot::Mutex::new(None),
            positive,
            secure: false,
        }
    }

    fn secure() -> Self {
        Self {
            secure: true,
            ..Self::new(true)
        }
    }

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.example.fixture".into(),
            window_id: "window-1".into(),
            generation: 1,
            display_name: "Fixture".into(),
            sensitivity: Sensitivity::None,
        }
    }
}

#[async_trait]
impl ComputerBackend for FixtureBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "sealed_boundary_fixture".into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: false,
            pointer_fallback: false,
        }
    }

    async fn observe(
        &self,
        _run_id: &str,
        observation_id: &str,
        target: &ComputerTarget,
        _limits: &ComputerUseLimits,
    ) -> ComputerResult<ComputerObservation> {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let element = |id: &str, enabled: bool, sensitivity: Sensitivity, value: Option<String>| {
            SemanticElement {
                element_id: id.into(),
                role: "text_field".into(),
                label: Some("Field".into()),
                value,
                bounds: None,
                enabled,
                focused: false,
                sensitivity,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }
        };
        Ok(ComputerObservation {
            observation_id: observation_id.to_string(),
            sequence,
            target: target.clone(),
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: if self.secure {
                vec![element(SECURE_ELEMENT, true, Sensitivity::Secure, None)]
            } else {
                vec![
                    element(
                        NAME_ELEMENT,
                        true,
                        Sensitivity::None,
                        self.value.lock().clone(),
                    ),
                    element(DISABLED_ELEMENT, false, Sensitivity::None, None),
                ]
            },
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        })
    }

    async fn act(
        &self,
        _run_id: &str,
        _observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        if let ComputerAction::SetValue { text, .. } = action {
            *self.value.lock() = Some(text.clone());
        }
        Ok(ActionOutcome::bounded(
            "fixture action",
            self.positive.then_some(true),
        ))
    }

    async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
        Ok(())
    }
}

struct Harness {
    _dir: TempDir,
    service: Arc<ComputerUseService>,
    owner: Uuid,
}

impl Harness {
    fn new(positive: bool) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = ComputerStore::open(dir.path()).expect("store");
        let service = Arc::new(ComputerUseService::new(
            Arc::new(FixtureBackend::new(positive)),
            store,
        ));
        Self {
            _dir: dir,
            service,
            owner: Uuid::new_v4(),
        }
    }

    /// A run authorized for several actions, so the grant's one-use desktop
    /// policy does not mask the kernel behaviour under test.
    async fn ready_run(&self) -> ComputerRun {
        let run = self
            .service
            .create_run(
                &Uuid::new_v4().to_string(),
                self.owner,
                None,
                FixtureBackend::target(),
                ComputerUseLimits::default(),
            )
            .expect("create run");
        let now = Utc::now();
        let run = self
            .service
            .authorize(
                &Uuid::new_v4().to_string(),
                &run.run_id,
                run.version,
                ActionGrant {
                    grant_id: Uuid::new_v4().to_string(),
                    run_id: run.run_id.clone(),
                    target: FixtureBackend::target(),
                    action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
                    issued_by: GrantIssuer::LocalUser,
                    issued_at: now,
                    expires_at: now + Duration::minutes(5),
                    uses_remaining: Some(8),
                    revoked_at: None,
                },
            )
            .expect("authorize");
        self.service
            .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
            .await
            .expect("observe");
        self.run(&run.run_id)
    }

    fn run(&self, run_id: &str) -> ComputerRun {
        self.service
            .get_run(run_id)
            .expect("load run")
            .expect("run exists")
    }

    fn context(&self, run: &ComputerRun) -> ComputerResult<ModelProposalContext> {
        ModelProposalContext::from_run(run, self.owner, self.service.capabilities())
    }

    /// Dispatch one action and capture the single host-issued verifying frame,
    /// exactly as an approved desktop dispatch does.
    async fn dispatch_and_verify(&self, run: &ComputerRun, text: &str) -> ComputerRun {
        let observation_id = run
            .current_observation
            .as_ref()
            .expect("current observation")
            .observation_id
            .clone();
        self.service
            .act(
                &Uuid::new_v4().to_string(),
                &run.run_id,
                run.version,
                &observation_id,
                ComputerAction::SetValue {
                    element_id: NAME_ELEMENT.into(),
                    text: text.into(),
                },
            )
            .await
            .expect("act");
        let acted = self.run(&run.run_id);
        self.service
            .observe_postcondition(&Uuid::new_v4().to_string(), &run.run_id, acted.version)
            .await
            .expect("postcondition observation");
        self.run(&run.run_id)
    }

    async fn observe_again(&self, run: &ComputerRun) -> ComputerRun {
        self.service
            .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
            .await
            .expect("observe");
        self.run(&run.run_id)
    }
}

fn action_json(observation_id: &str, element_id: &str, text: &str) -> RawModelProposal {
    RawModelProposal::new(
        serde_json::json!({
            "observation_id": observation_id,
            "action_type": "set_value",
            "element_id": element_id,
            "text": text,
            "summary": "Enter the visible name"
        })
        .to_string(),
    )
}

fn complete_json(observation_id: &str) -> RawModelProposal {
    RawModelProposal::new(
        serde_json::json!({
            "observation_id": observation_id,
            "action_type": "complete",
            "summary": "The visible objective is satisfied"
        })
        .to_string(),
    )
}

fn current_frame(run: &ComputerRun) -> String {
    run.current_observation
        .as_ref()
        .expect("current observation")
        .observation_id
        .clone()
}

fn code(error: &ComputerError) -> ComputerErrorCode {
    error.code
}

// ---------------------------------------------------------------------------
// #457 — nothing but a sealed proposal is an authority
// ---------------------------------------------------------------------------

/// Attack: hand the boundary a fabricated `complete` on a fresh run that has
/// never dispatched anything. This is the exact shape #457 reports as accepted
/// by the pre-change public seam.
#[tokio::test]
async fn fabricated_completion_on_a_fresh_run_fails_closed() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let context = harness.context(&run).expect("context");
    let error = accept_model_proposal(&context, &complete_json(&current_frame(&run)))
        .expect_err("a fresh run has no receipt");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);

    // No state moved.
    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    assert_eq!(after.action_count, 0);
    assert!(after.last_receipt.is_none());
}

/// Attack: propose an action against an element the observation marks
/// disabled, or one it marks secure, or one it does not advertise the action
/// for. Generic staging did not enforce any of these.
#[tokio::test]
async fn model_only_element_restrictions_are_enforced_at_the_boundary() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let context = harness.context(&run).expect("context");

    let disabled = accept_model_proposal(&context, &action_json(&frame, DISABLED_ELEMENT, "x"))
        .expect_err("disabled element");
    assert_eq!(code(&disabled), ComputerErrorCode::ForbiddenAction);

    let missing = accept_model_proposal(&context, &action_json(&frame, "no-such-element", "x"))
        .expect_err("unknown element");
    assert_eq!(code(&missing), ComputerErrorCode::StaleObservation);

    let unadvertised = RawModelProposal::new(
        serde_json::json!({
            "observation_id": frame,
            "action_type": "invoke",
            "element_id": NAME_ELEMENT,
            "summary": "invoke a field that only advertises set_value"
        })
        .to_string(),
    );
    let unadvertised =
        accept_model_proposal(&context, &unadvertised).expect_err("unadvertised action");
    assert_eq!(code(&unadvertised), ComputerErrorCode::ForbiddenAction);
}

/// A frame carrying a hard-denied element is refused before it is ever
/// exposed, so a model cannot be shown a sensitive element to propose against
/// in the first place. This is the stronger property the normalizer's own
/// sensitivity check backs up in depth.
#[tokio::test]
async fn a_frame_containing_a_sensitive_element_is_never_exposed() {
    let dir = TempDir::new().expect("temp dir");
    let store = ComputerStore::open(dir.path()).expect("store");
    let service = ComputerUseService::new(Arc::new(FixtureBackend::secure()), store);
    let owner = Uuid::new_v4();
    let run = service
        .create_run(
            &Uuid::new_v4().to_string(),
            owner,
            None,
            FixtureBackend::target(),
            ComputerUseLimits::default(),
        )
        .expect("create run");
    let now = Utc::now();
    let run = service
        .authorize(
            &Uuid::new_v4().to_string(),
            &run.run_id,
            run.version,
            ActionGrant {
                grant_id: Uuid::new_v4().to_string(),
                run_id: run.run_id.clone(),
                target: FixtureBackend::target(),
                action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
                issued_by: GrantIssuer::LocalUser,
                issued_at: now,
                expires_at: now + Duration::minutes(5),
                uses_remaining: Some(8),
                revoked_at: None,
            },
        )
        .expect("authorize");
    let error = service
        .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
        .await
        .expect_err("a hard-denied element must not be exposed");
    assert_eq!(code(&error), ComputerErrorCode::SensitiveSurface);

    // With no exposed frame there is no context, so no proposal is possible.
    let run = service.get_run(&run.run_id).expect("load").expect("run");
    assert!(ModelProposalContext::from_run(&run, owner, service.capabilities()).is_err());
}

/// Attack: reach past the semantic kernel with a pointer click, a key chord,
/// or a wait. These stay operator-only whatever the grant or backend says.
#[tokio::test]
async fn operator_only_action_kinds_are_never_model_proposable() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let context = harness.context(&run).expect("context");

    for action_type in ["pointer_click", "key_chord", "wait", "shell", ""] {
        let raw = RawModelProposal::new(
            serde_json::json!({
                "observation_id": frame,
                "action_type": action_type,
                "summary": "escape the semantic kernel"
            })
            .to_string(),
        );
        match accept_model_proposal(&context, &raw) {
            Ok(accepted) => panic!("`{action_type}` was accepted as {:?}", accepted.intent()),
            Err(error) => assert!(
                matches!(
                    error.code,
                    ComputerErrorCode::ForbiddenAction | ComputerErrorCode::InvalidRequest
                ),
                "`{action_type}` refused with an unexpected code: {error:?}"
            ),
        }
    }
}

/// Attack: exploit `serde_json`'s last-key-wins behaviour so the payload a
/// validator reads differs from the payload an applier reads. Also covers
/// unknown keys, truncation, prose, and trailing content.
#[tokio::test]
async fn malformed_duplicate_key_and_unknown_field_payloads_fail_closed() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let context = harness.context(&run).expect("context");

    // Two `element_id` keys: benign first, hostile second.
    let duplicate = format!(
        r#"{{"observation_id":"{frame}","action_type":"set_value","element_id":"{NAME_ELEMENT}","element_id":"{SECURE_ELEMENT}","text":"x","summary":"duplicate key"}}"#
    );
    let error = accept_model_proposal(&context, &RawModelProposal::new(duplicate))
        .expect_err("duplicate keys");
    assert_eq!(code(&error), ComputerErrorCode::InvalidRequest);
    assert!(error.message.contains("duplicate key"));

    let unknown = format!(
        r#"{{"observation_id":"{frame}","action_type":"set_value","element_id":"{NAME_ELEMENT}","text":"x","summary":"s","shell":"whoami"}}"#
    );
    assert_eq!(
        code(
            &accept_model_proposal(&context, &RawModelProposal::new(unknown))
                .expect_err("unknown key")
        ),
        ComputerErrorCode::InvalidRequest
    );

    for hostile in [
        "{",
        "",
        "Sure! Here is the action you asked for.",
        "```json\n{\"observation_id\":\"x\"}\n```",
    ] {
        assert_eq!(
            code(
                &accept_model_proposal(&context, &RawModelProposal::new(hostile))
                    .expect_err("malformed payload")
            ),
            ComputerErrorCode::InvalidRequest
        );
    }

    // Trailing content after a well-formed object.
    let trailing = format!(
        r#"{{"observation_id":"{frame}","action_type":"complete","summary":"s"}} {{"observation_id":"{frame}"}}"#
    );
    assert_eq!(
        code(
            &accept_model_proposal(&context, &RawModelProposal::new(trailing))
                .expect_err("trailing")
        ),
        ComputerErrorCode::InvalidRequest
    );
}

/// Attack: propose against a frame that is no longer current.
#[tokio::test]
async fn stale_frame_proposals_are_refused() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let stale_frame = current_frame(&run);
    let run = harness.observe_again(&run).await;
    let context = harness.context(&run).expect("context");
    assert_eq!(
        code(
            &accept_model_proposal(&context, &action_json(&stale_frame, NAME_ELEMENT, "x"))
                .expect_err("stale frame")
        ),
        ComputerErrorCode::StaleObservation
    );
}

/// Attack: present a proposal that was validly sealed, but only after the run
/// has moved on. The seal is a snapshot; the live record is the authority.
#[tokio::test]
async fn a_sealed_proposal_cannot_apply_after_re_observation_or_cancellation() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let context = harness.context(&run).expect("context");
    let sealed = accept_model_proposal(
        &context,
        &action_json(&current_frame(&run), NAME_ELEMENT, "Ada"),
    )
    .expect("seal");

    let moved = harness.observe_again(&run).await;
    let error = sealed
        .authorize_against(&moved, harness.owner)
        .expect_err("seal is stale after re-observation");
    assert_eq!(code(&error), ComputerErrorCode::Conflict);

    // And after cancellation the same seal is equally dead.
    harness
        .service
        .cancel(&Uuid::new_v4().to_string(), &run.run_id)
        .await
        .expect("cancel");
    let cancelled = harness.run(&run.run_id);
    assert!(sealed.authorize_against(&cancelled, harness.owner).is_err());
    assert!(cancelled.last_receipt.is_none());
}

/// Attack: spend one seal in another session, or against another run.
#[tokio::test]
async fn a_sealed_proposal_is_bound_to_its_run_and_owner() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let context = harness.context(&run).expect("context");
    let sealed = accept_model_proposal(
        &context,
        &action_json(&current_frame(&run), NAME_ELEMENT, "Ada"),
    )
    .expect("seal");

    assert_eq!(
        code(
            &sealed
                .authorize_against(&run, Uuid::new_v4())
                .expect_err("wrong owner")
        ),
        ComputerErrorCode::Unauthorized
    );
    assert!(sealed.authorize_against(&run, harness.owner).is_ok());
}

/// A context cannot even be built for a session that does not own the run, so
/// a cross-session proposal never reaches the normalizer.
#[tokio::test]
async fn a_foreign_session_cannot_build_a_proposal_context() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let error =
        ModelProposalContext::from_run(&run, Uuid::new_v4(), harness.service.capabilities())
            .expect_err("foreign session");
    assert_eq!(code(&error), ComputerErrorCode::Unauthorized);
}

// ---------------------------------------------------------------------------
// #456 — completion is bound to the exact current frame and receipt
// ---------------------------------------------------------------------------

/// The legitimate path: dispatch, let the host capture the one verifying
/// frame, then complete against that exact frame.
#[tokio::test]
async fn a_verified_receipt_on_the_current_frame_permits_completion() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
    assert!(run.last_receipt.is_some(), "dispatch minted a receipt");

    let context = harness.context(&run).expect("context");
    let sealed =
        accept_model_proposal(&context, &complete_json(&current_frame(&run))).expect("seal");
    let AcceptedIntent::Complete { evidence } = sealed.intent() else {
        panic!("expected a completion intent");
    };
    let evidence = evidence.clone();
    sealed
        .authorize_against(&run, harness.owner)
        .expect("evidence still holds");
    let completed = harness
        .service
        .complete_verified(
            &Uuid::new_v4().to_string(),
            &run.run_id,
            run.version,
            &evidence,
        )
        .expect("completion is authorized");
    assert_eq!(completed.state, ComputerRunState::Completed);
    assert!(completed
        .grant
        .as_ref()
        .is_some_and(|grant| grant.revoked_at.is_some()));
}

/// The #456 unsafe sequence, verbatim: dispatch a positive action on frame 1,
/// observe frame 2, then propose `complete` bound to frame 2. Current-frame
/// binding alone would pass. It must not.
#[tokio::test]
async fn dispatch_then_re_observe_then_complete_is_refused() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
    assert!(run.last_receipt.is_some());

    // One further ordinary observation is enough to destroy the proof.
    let run = harness.observe_again(&run).await;
    assert!(
        run.last_receipt.is_none(),
        "an ordinary observation must clear completion evidence"
    );

    let context = harness.context(&run).expect("context");
    let error = accept_model_proposal(&context, &complete_json(&current_frame(&run)))
        .expect_err("evidence did not verify this frame");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);

    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    assert!(!after.state.is_terminal());
}

/// Attack: capture evidence while it is valid, then spend it one frame later.
#[tokio::test]
async fn captured_evidence_cannot_be_replayed_onto_a_later_frame() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
    let context = harness.context(&run).expect("context");
    let sealed =
        accept_model_proposal(&context, &complete_json(&current_frame(&run))).expect("seal");
    let AcceptedIntent::Complete { evidence } = sealed.intent() else {
        panic!("expected a completion intent");
    };
    let evidence = evidence.clone();

    let moved = harness.observe_again(&run).await;
    let error = harness
        .service
        .complete_verified(
            &Uuid::new_v4().to_string(),
            &moved.run_id,
            moved.version,
            &evidence,
        )
        .expect_err("replayed evidence");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);
    assert_eq!(harness.run(&run.run_id).state, ComputerRunState::Ready);
}

/// Attack: forge evidence field by field — wrong receipt id, wrong action
/// fingerprint, wrong frame, wrong authority revision.
#[tokio::test]
async fn mismatched_receipt_fingerprint_frame_or_revision_is_refused() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
    let context = harness.context(&run).expect("context");
    let sealed =
        accept_model_proposal(&context, &complete_json(&current_frame(&run))).expect("seal");
    let AcceptedIntent::Complete { evidence } = sealed.intent() else {
        panic!("expected a completion intent");
    };

    let mut wrong_receipt = evidence.clone();
    wrong_receipt.receipt_id = "receipt-forged".into();
    let mut wrong_fingerprint = evidence.clone();
    wrong_fingerprint.action_fingerprint = "0".repeat(64);
    let mut wrong_frame = evidence.clone();
    wrong_frame.frame.sequence = wrong_frame.frame.sequence.saturating_add(1);
    let mut wrong_id = evidence.clone();
    wrong_id.frame.observation_id = "observation-forged".into();
    let mut wrong_epoch = evidence.clone();
    wrong_epoch.control_epoch = wrong_epoch.control_epoch.saturating_add(1);

    for (label, forged) in [
        ("receipt id", wrong_receipt),
        ("action fingerprint", wrong_fingerprint),
        ("frame sequence", wrong_frame),
        ("frame id", wrong_id),
        ("authority revision", wrong_epoch),
    ] {
        match harness.service.complete_verified(
            &Uuid::new_v4().to_string(),
            &run.run_id,
            run.version,
            &forged,
        ) {
            Ok(completed) => panic!("forged {label} completed the run: {:?}", completed.state),
            Err(error) => assert_eq!(
                error.code,
                ComputerErrorCode::UnverifiedCompletion,
                "forged {label} refused with an unexpected code"
            ),
        }
        // Each forgery leaves the run untouched.
        assert_eq!(harness.run(&run.run_id).state, ComputerRunState::Ready);
    }
}

/// A dispatch the backend does not report as meeting its postcondition can
/// never become completion evidence, however current its frame is.
#[tokio::test]
async fn a_non_positive_outcome_never_becomes_completion_evidence() {
    let harness = Harness::new(false);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
    assert!(
        run.last_receipt.is_none(),
        "an unconfirmed postcondition must not survive as evidence"
    );
    let context = harness.context(&run).expect("context");
    assert_eq!(
        code(
            &accept_model_proposal(&context, &complete_json(&current_frame(&run)))
                .expect_err("no positive evidence")
        ),
        ComputerErrorCode::UnverifiedCompletion
    );
}

/// Steering, takeover, and cancellation each move the authority revision, and
/// each must strand the evidence bound to the previous one.
#[tokio::test]
async fn authority_changes_clear_completion_evidence() {
    for (label, revoke) in [("pause", false), ("take_over", true)] {
        let harness = Harness::new(true);
        let run = harness.ready_run().await;
        let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
        assert!(
            run.last_receipt.is_some(),
            "{label}: dispatch minted evidence"
        );

        if revoke {
            harness
                .service
                .take_over(&Uuid::new_v4().to_string(), &run.run_id, run.version)
                .await
                .expect("take over");
        } else {
            harness
                .service
                .pause(&Uuid::new_v4().to_string(), &run.run_id, run.version)
                .await
                .expect("pause");
        }
        let after = harness.run(&run.run_id);
        assert!(
            after.last_receipt.is_none(),
            "{label} must clear completion evidence"
        );
        assert!(after.current_observation.is_none());
    }
}

/// A process restart strands every run. Evidence written before the restart
/// must not survive into the recovered record.
#[tokio::test]
async fn restart_recovery_strands_completion_evidence() {
    let dir = TempDir::new().expect("temp dir");
    let owner = Uuid::new_v4();
    let run_id = {
        let store = ComputerStore::open(dir.path()).expect("store");
        let service = Arc::new(ComputerUseService::new(
            Arc::new(FixtureBackend::new(true)),
            store,
        ));
        let harness = Harness {
            _dir: TempDir::new().expect("scratch"),
            service: service.clone(),
            owner,
        };
        let run = harness.ready_run().await;
        let run = harness.dispatch_and_verify(&run, "Ada Lovelace").await;
        assert!(run.last_receipt.is_some());
        run.run_id
    };

    // Reopening the store is what a restart looks like to the ledger.
    let reopened = ComputerStore::open(dir.path()).expect("reopen store");
    let service = ComputerUseService::new(Arc::new(FixtureBackend::new(true)), reopened);
    let recovered = service.get_run(&run_id).expect("load").expect("run exists");
    assert_eq!(recovered.state, ComputerRunState::Interrupted);
    assert!(recovered.last_receipt.is_none());
    assert!(recovered.last_outcome.is_none());
    assert!(recovered.current_observation.is_none());
    assert!(ModelProposalContext::from_run(&recovered, owner, service.capabilities()).is_err());
}

/// Two identical normalized proposals for one frame: the second is a
/// duplicate, whether it arrives from a retry or a replay.
#[tokio::test]
async fn duplicate_normalized_proposals_share_one_fingerprint() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let context = harness.context(&run).expect("context");
    let first =
        accept_model_proposal(&context, &action_json(&frame, NAME_ELEMENT, "Ada")).expect("first");
    let second =
        accept_model_proposal(&context, &action_json(&frame, NAME_ELEMENT, "Ada")).expect("second");
    assert_eq!(first.proposal_fingerprint(), second.proposal_fingerprint());
    // Distinct seals even for identical content, so single-use accounting can
    // tell two presentations apart.
    assert_ne!(first.nonce(), second.nonce());

    let different = accept_model_proposal(&context, &action_json(&frame, NAME_ELEMENT, "Grace"))
        .expect("other");
    assert_ne!(
        first.proposal_fingerprint(),
        different.proposal_fingerprint()
    );
}

/// Concurrent application of two seals minted from the same snapshot: the
/// first to land wins and the second is refused, because staging bumps the run
/// version the second seal is bound to.
#[tokio::test]
async fn concurrent_seals_from_one_snapshot_cannot_both_apply() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let context = harness.context(&run).expect("context");
    let first =
        accept_model_proposal(&context, &action_json(&frame, NAME_ELEMENT, "Ada")).expect("first");
    let second = accept_model_proposal(&context, &action_json(&frame, NAME_ELEMENT, "Grace"))
        .expect("second");

    let AcceptedIntent::Action { action, .. } = first.intent().clone() else {
        panic!("expected an action intent");
    };
    first
        .authorize_against(&run, harness.owner)
        .expect("first seal is live");
    harness
        .service
        .act(
            &Uuid::new_v4().to_string(),
            &run.run_id,
            run.version,
            &frame,
            action,
        )
        .await
        .expect("dispatch the first");

    let moved = harness.run(&run.run_id);
    assert_eq!(
        code(
            &second
                .authorize_against(&moved, harness.owner)
                .expect_err("second seal lost the race")
        ),
        ComputerErrorCode::Conflict
    );
}

/// Typed refusals reach operators and SDK consumers through the run's existing
/// durable event journal, not a second ledger.
#[tokio::test]
async fn refusals_are_journaled_with_their_typed_code() {
    let harness = Harness::new(true);
    let run = harness.ready_run().await;
    let context = harness.context(&run).expect("context");
    let error = accept_model_proposal(&context, &complete_json(&current_frame(&run)))
        .expect_err("no evidence");
    harness
        .service
        .record_proposal_refusal(&run.run_id, "apply_model_proposal", &error);

    let page = harness
        .service
        .session_run_events(harness.owner, &run.run_id, None, 64)
        .expect("event page");
    let refusal = page
        .entries
        .iter()
        .find(|entry| entry.disposition == "refused")
        .expect("refusal is journaled");
    assert_eq!(refusal.operation, "apply_model_proposal");
    assert_eq!(
        refusal.error_code,
        Some(ComputerErrorCode::UnverifiedCompletion)
    );
    // Secret-free: the journal carries the code, never the model's text or the
    // observed content that produced it.
    assert!(refusal.observation_id.is_none());

    // The refusal did not move the run.
    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    assert_eq!(after.version, run.version);
}
