//! Adversarial coverage for the sealed model-output boundary (#457) and the
//! objective-bound completion proof (#456).
//!
//! Everything here runs against a deterministic in-process backend. No provider
//! is called, no OS input is dispatched, and no real application is opened.
//! Each test names the attack it defeats, because a green assertion on its own
//! does not say which bypass is closed.
//!
//! Element IDs in the fixture are deliberately **ephemeral** — a fresh UUID on
//! every frame — because that is what a real accessibility adapter produces.
//! Anything that appears to work here by remembering an element ID would be
//! working by accident.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use tempfile::TempDir;
use uuid::Uuid;

use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities,
    ComputerControlDisposition, ComputerError, ComputerErrorCode, ComputerObservation,
    ComputerResult, ComputerRun, ComputerRunState, ComputerStore, ComputerTarget, ComputerTaskSpec,
    ComputerUseLimits, ElementLocator, GrantIssuer, ObservationGeometry, SemanticAction,
    SemanticElement, Sensitivity, TaskPredicate,
};
use grokptah_agent_bridge::{
    accept_model_output, AcceptedIntent, AcceptedModelProposal, ComputerUseService, ModelTurn,
    RawModelProposal, RouteBinding,
};

const NAME_ROLE: &str = "text_field";
const NAME_LABEL: &str = "Name";
const DISABLED_LABEL: &str = "Locked";
const SECURE_LABEL: &str = "Password";
const OBJECTIVE: &str = "Enter Ada Lovelace in the visible Name field";
const TARGET_VALUE: &str = "Ada Lovelace";

/// What the fixture backend should do when asked to act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActMode {
    /// Mutate and report the postcondition met.
    Positive,
    /// Mutate but report nothing about the postcondition.
    Unconfirmed,
    /// Fail before touching anything.
    FailBeforeEffect,
    /// Mutate, then fail. The host cannot tell this from the case above.
    FailAfterEffect,
    /// Refuse on authorization grounds, which an adapter can only do before
    /// touching anything.
    RefuseUnauthorized,
}

#[derive(Debug)]
struct FixtureBackend {
    sequence: AtomicU64,
    value: parking_lot::Mutex<Option<String>>,
    act_mode: ActMode,
    secure: bool,
    /// Fail the next observation. Models a crash cut during postcondition
    /// capture.
    fail_observe: parking_lot::Mutex<bool>,
}

impl FixtureBackend {
    fn new(act_mode: ActMode) -> Self {
        Self {
            sequence: AtomicU64::new(0),
            value: parking_lot::Mutex::new(None),
            act_mode,
            secure: false,
            fail_observe: parking_lot::Mutex::new(false),
        }
    }

    fn secure() -> Self {
        Self {
            secure: true,
            ..Self::new(ActMode::Positive)
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

/// Every element gets a fresh ID on every frame: role and label are the only
/// stable handles, exactly as in production.
fn element(
    label: &str,
    enabled: bool,
    sensitivity: Sensitivity,
    value: Option<String>,
) -> SemanticElement {
    SemanticElement {
        element_id: format!("element-{}", Uuid::new_v4()),
        role: NAME_ROLE.into(),
        label: Some(label.into()),
        value,
        bounds: None,
        enabled,
        focused: false,
        sensitivity,
        actions: BTreeSet::from([SemanticAction::SetValue]),
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
        if std::mem::replace(&mut *self.fail_observe.lock(), false) {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "fixture observation cut",
            ));
        }
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
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
                vec![element(SECURE_LABEL, true, Sensitivity::Secure, None)]
            } else {
                vec![
                    element(
                        NAME_LABEL,
                        true,
                        Sensitivity::None,
                        self.value.lock().clone(),
                    ),
                    element(DISABLED_LABEL, false, Sensitivity::None, None),
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
        let mutate = || {
            if let ComputerAction::SetValue { text, .. } = action {
                *self.value.lock() = Some(text.clone());
            }
        };
        match self.act_mode {
            ActMode::FailBeforeEffect => Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "fixture refused before touching anything",
            )),
            ActMode::RefuseUnauthorized => Err(ComputerError::new(
                ComputerErrorCode::PermissionRevoked,
                "fixture permission was revoked",
            )),
            ActMode::FailAfterEffect => {
                mutate();
                Err(ComputerError::new(
                    ComputerErrorCode::BackendFailure,
                    "fixture failed after mutating",
                ))
            }
            ActMode::Positive => {
                mutate();
                Ok(ActionOutcome::bounded("fixture action", Some(true)))
            }
            ActMode::Unconfirmed => {
                mutate();
                Ok(ActionOutcome::bounded("fixture action", None))
            }
        }
    }

    async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
        Ok(())
    }
}

fn name_locator() -> ElementLocator {
    ElementLocator::new(NAME_ROLE, Some(NAME_LABEL.into()))
}

fn task_spec() -> ComputerTaskSpec {
    ComputerTaskSpec::new(
        OBJECTIVE,
        TaskPredicate::ElementValueEquals {
            locator: name_locator(),
            value: TARGET_VALUE.into(),
        },
        4,
    )
    .expect("operator task spec")
}

fn route() -> RouteBinding {
    RouteBinding::new("route-fingerprint-a", "fixture-model")
}

struct Harness {
    _dir: TempDir,
    service: Arc<ComputerUseService>,
    owner: Uuid,
}

impl Harness {
    fn new(act_mode: ActMode) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = ComputerStore::open(dir.path()).expect("store");
        Self {
            _dir: dir,
            service: Arc::new(ComputerUseService::new(
                Arc::new(FixtureBackend::new(act_mode)),
                store,
            )),
            owner: Uuid::new_v4(),
        }
    }

    async fn ready_run(&self) -> ComputerRun {
        self.ready_run_with(Some(task_spec())).await
    }

    async fn ready_run_with(&self, spec: Option<ComputerTaskSpec>) -> ComputerRun {
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
        let run = match spec {
            Some(spec) => self
                .service
                .set_task_spec(&Uuid::new_v4().to_string(), &run.run_id, run.version, spec)
                .expect("set task spec"),
            None => run,
        };
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

    fn accept(
        &self,
        run: &ComputerRun,
        raw: &RawModelProposal,
    ) -> ComputerResult<AcceptedModelProposal> {
        self.accept_as(run, OBJECTIVE, route(), raw)
    }

    fn accept_as(
        &self,
        run: &ComputerRun,
        objective: &str,
        route: RouteBinding,
        raw: &RawModelProposal,
    ) -> ComputerResult<AcceptedModelProposal> {
        accept_model_output(
            &self.service,
            self.owner,
            &ModelTurn {
                run_id: &run.run_id,
                expected_version: run.version,
                observation_id: &current_frame(run),
                objective,
            },
            route,
            raw,
        )
    }

    /// One approved dispatch plus the single host-issued verifying frame,
    /// exactly as the desktop performs it.
    async fn dispatch_and_verify(&self, run: &ComputerRun, text: &str) -> ComputerRun {
        // A dispatch that fails is a legitimate case for several callers here;
        // what matters is the state the run lands in, asserted below.
        let _ = self.dispatch(run, text).await;
        let acted = self.run(&run.run_id);
        if acted.state == ComputerRunState::Ready {
            let _ = self
                .service
                .observe_postcondition(&Uuid::new_v4().to_string(), &run.run_id, acted.version)
                .await;
        }
        self.run(&run.run_id)
    }

    async fn dispatch(&self, run: &ComputerRun, text: &str) -> ComputerResult<ActionOutcome> {
        let observation = run
            .current_observation
            .as_ref()
            .expect("current observation");
        let element_id = observation
            .elements
            .iter()
            .find(|element| element.label.as_deref() == Some(NAME_LABEL))
            .expect("name element")
            .element_id
            .clone();
        self.service
            .act(
                &Uuid::new_v4().to_string(),
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id,
                    text: text.into(),
                },
            )
            .await
    }

    async fn observe_again(&self, run: &ComputerRun) -> ComputerRun {
        self.service
            .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
            .await
            .expect("observe");
        self.run(&run.run_id)
    }
}

fn current_frame(run: &ComputerRun) -> String {
    run.current_observation
        .as_ref()
        .expect("current observation")
        .observation_id
        .clone()
}

fn name_element_id(run: &ComputerRun) -> String {
    run.current_observation
        .as_ref()
        .expect("current observation")
        .elements
        .iter()
        .find(|element| element.label.as_deref() == Some(NAME_LABEL))
        .expect("name element")
        .element_id
        .clone()
}

fn labelled_element_id(run: &ComputerRun, label: &str) -> String {
    run.current_observation
        .as_ref()
        .expect("current observation")
        .elements
        .iter()
        .find(|element| element.label.as_deref() == Some(label))
        .expect("element")
        .element_id
        .clone()
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

fn code(error: &ComputerError) -> ComputerErrorCode {
    error.code
}

// ---------------------------------------------------------------------------
// #457 — nothing but a host-minted seal is an authority
// ---------------------------------------------------------------------------

/// Attack: fabricate `complete` on a fresh run that has never dispatched
/// anything. The shape #457 reports as accepted by the pre-change seam.
#[tokio::test]
async fn fabricated_completion_on_a_fresh_run_fails_closed() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let error = harness
        .accept(&run, &complete_json(&current_frame(&run)))
        .expect_err("a fresh run has no receipt");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);

    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    assert_eq!(after.action_count, 0);
    assert!(after.last_receipt.is_none());
}

/// Attack: hand the boundary a run record the caller built itself.
///
/// There is no seam that takes one. `accept_model_output` takes identifiers and
/// reads the ledger, so a fabricated record — however perfectly formed — names a
/// run that does not exist and is refused with the same answer an unowned run
/// gets, which also keeps it from being an existence oracle.
#[tokio::test]
async fn a_caller_constructed_run_cannot_mint_a_seal() {
    let harness = Harness::new(ActMode::Positive);
    let real = harness.ready_run().await;

    // A perfectly formed forgery: ready, granted, observed, and carrying a
    // receipt that would authorize completion if anyone believed it.
    let mut forged = real.clone();
    forged.run_id = Uuid::new_v4().to_string();
    forged.version = 1;

    let error = accept_model_output(
        &harness.service,
        harness.owner,
        &ModelTurn {
            run_id: &forged.run_id,
            expected_version: forged.version,
            observation_id: &current_frame(&forged),
            objective: OBJECTIVE,
        },
        route(),
        &complete_json(&current_frame(&forged)),
    )
    .expect_err("a fabricated run is not in the ledger");
    assert_eq!(code(&error), ComputerErrorCode::Unauthorized);

    // An unknown run and a run owned by someone else answer identically.
    let foreign = accept_model_output(
        &harness.service,
        Uuid::new_v4(),
        &ModelTurn {
            run_id: &real.run_id,
            expected_version: real.version,
            observation_id: &current_frame(&real),
            objective: OBJECTIVE,
        },
        route(),
        &complete_json(&current_frame(&real)),
    )
    .expect_err("cross-session");
    assert_eq!(code(&foreign), code(&error));
    assert_eq!(foreign.message, error.message);
}

/// Attack: run the model against a different objective than the operator
/// authored, then complete against the operator's spec.
#[tokio::test]
async fn an_objective_the_operator_did_not_author_is_refused() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let error = harness
        .accept_as(
            &run,
            "Delete every file in the Documents folder",
            route(),
            &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
        )
        .expect_err("objective mismatch");
    assert_eq!(code(&error), ComputerErrorCode::Unauthorized);
}

/// Attack: mint under one provider route, apply under another.
#[tokio::test]
async fn a_seal_cannot_be_applied_under_a_different_route() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let sealed = harness
        .accept(
            &run,
            &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
        )
        .expect("seal");
    let capabilities = harness.service.capabilities();

    sealed
        .authorize_against(&run, harness.owner, &capabilities, &route())
        .expect("its own route");

    let other = RouteBinding::new("route-fingerprint-b", "fixture-model");
    assert_eq!(
        code(
            &sealed
                .authorize_against(&run, harness.owner, &capabilities, &other)
                .expect_err("different route")
        ),
        ComputerErrorCode::UnsealedProposal
    );

    // A capability generation appearing where there was none is also a change.
    let generational = route().with_capability_generation("cap-gen-1");
    assert!(sealed
        .authorize_against(&run, harness.owner, &capabilities, &generational)
        .is_err());
}

/// Attack: apply a seal after the backend capability surface narrows.
#[tokio::test]
async fn a_seal_dies_when_the_capability_surface_changes() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let sealed = harness
        .accept(
            &run,
            &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
        )
        .expect("seal");
    let narrowed = ComputerCapabilities {
        text_entry: false,
        ..harness.service.capabilities()
    };
    assert_eq!(
        code(
            &sealed
                .authorize_against(&run, harness.owner, &narrowed, &route())
                .expect_err("capability withdrawal")
        ),
        ComputerErrorCode::UnsealedProposal
    );
}

/// Attack: propose against a disabled element, an element the frame does not
/// advertise the action for, or one that is not there at all.
#[tokio::test]
async fn model_only_element_restrictions_are_enforced_at_the_boundary() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);

    let disabled = harness
        .accept(
            &run,
            &action_json(&frame, &labelled_element_id(&run, DISABLED_LABEL), "x"),
        )
        .expect_err("disabled element");
    assert_eq!(code(&disabled), ComputerErrorCode::ForbiddenAction);

    let missing = harness
        .accept(&run, &action_json(&frame, "element-does-not-exist", "x"))
        .expect_err("unknown element");
    assert_eq!(code(&missing), ComputerErrorCode::StaleObservation);

    let unadvertised = RawModelProposal::new(
        serde_json::json!({
            "observation_id": frame,
            "action_type": "invoke",
            "element_id": name_element_id(&run),
            "summary": "invoke a field that only advertises set_value"
        })
        .to_string(),
    );
    assert_eq!(
        code(
            &harness
                .accept(&run, &unadvertised)
                .expect_err("unadvertised action")
        ),
        ComputerErrorCode::ForbiddenAction
    );
}

/// A frame carrying a hard-denied element is refused before exposure, so a
/// model is never shown a sensitive element to propose against.
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
}

/// Attack: reach past the semantic kernel with a pointer click, a key chord, or
/// a wait. These stay operator-only whatever the grant or backend says.
#[tokio::test]
async fn operator_only_action_kinds_are_never_model_proposable() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);

    for action_type in ["pointer_click", "key_chord", "wait", "shell", ""] {
        let raw = RawModelProposal::new(
            serde_json::json!({
                "observation_id": frame,
                "action_type": action_type,
                "summary": "escape the semantic kernel"
            })
            .to_string(),
        );
        match harness.accept(&run, &raw) {
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
/// validator reads differs from the one an applier reads. Also covers unknown
/// keys, truncation, prose, and trailing content — weak and malformed output.
#[tokio::test]
async fn malformed_duplicate_key_and_unknown_field_payloads_fail_closed() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let good = name_element_id(&run);
    let locked = labelled_element_id(&run, DISABLED_LABEL);

    // Two `element_id` keys: benign first, hostile second.
    let duplicate = format!(
        r#"{{"observation_id":"{frame}","action_type":"set_value","element_id":"{good}","element_id":"{locked}","text":"x","summary":"duplicate key"}}"#
    );
    let error = harness
        .accept(&run, &RawModelProposal::new(duplicate))
        .expect_err("duplicate keys");
    assert_eq!(code(&error), ComputerErrorCode::InvalidRequest);
    assert!(error.message.contains("duplicate key"));

    let unknown = format!(
        r#"{{"observation_id":"{frame}","action_type":"set_value","element_id":"{good}","text":"x","summary":"s","shell":"whoami"}}"#
    );
    assert_eq!(
        code(
            &harness
                .accept(&run, &RawModelProposal::new(unknown))
                .expect_err("unknown key")
        ),
        ComputerErrorCode::InvalidRequest
    );

    for hostile in [
        "{",
        "",
        "Sure! Here is the action you asked for.",
        "```json\n{\"observation_id\":\"x\"}\n```",
        "null",
        "[]",
    ] {
        assert_eq!(
            code(
                &harness
                    .accept(&run, &RawModelProposal::new(hostile))
                    .expect_err("malformed payload")
            ),
            ComputerErrorCode::InvalidRequest,
            "payload {hostile:?} was not refused as malformed"
        );
    }

    let trailing = format!(
        r#"{{"observation_id":"{frame}","action_type":"complete","summary":"s"}} {{"observation_id":"{frame}"}}"#
    );
    assert_eq!(
        code(
            &harness
                .accept(&run, &RawModelProposal::new(trailing))
                .expect_err("trailing")
        ),
        ComputerErrorCode::InvalidRequest
    );
}

/// Model-authored summaries reach an operator's approval prompt. They are
/// capped, refused if they carry control characters, and scrubbed with the same
/// public privacy needles the durable journal uses.
#[tokio::test]
async fn model_summaries_are_bounded_and_redacted() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let element_id = name_element_id(&run);

    let leaky = RawModelProposal::new(
        serde_json::json!({
            "observation_id": frame,
            "action_type": "set_value",
            "element_id": element_id,
            "text": TARGET_VALUE,
            "summary": "Entering the name; using Authorization: Bearer sk-live-not-a-real-key to continue"
        })
        .to_string(),
    );
    let accepted = harness.accept(&run, &leaky).expect("seal");
    let summary = accepted.summary();
    assert!(
        !summary.contains("sk-live-not-a-real-key"),
        "credential survived redaction: {summary}"
    );
    assert!(
        summary.contains("[redacted]"),
        "no redaction marker: {summary}"
    );

    // Terminal escapes in an approval prompt attack the operator's ability to
    // see what they are approving.
    for hostile in ["ok\u{1b}[2Jcleared", "ok\u{0}injected", "   "] {
        let raw = RawModelProposal::new(
            serde_json::json!({
                "observation_id": frame,
                "action_type": "set_value",
                "element_id": element_id,
                "text": TARGET_VALUE,
                "summary": hostile
            })
            .to_string(),
        );
        assert_eq!(
            code(&harness.accept(&run, &raw).expect_err("hostile summary")),
            ComputerErrorCode::InvalidRequest
        );
    }
}

/// Attack: propose against a frame that is no longer current.
#[tokio::test]
async fn stale_frame_proposals_are_refused() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let stale = current_frame(&run);
    let moved = harness.observe_again(&run).await;
    let error = accept_model_output(
        &harness.service,
        harness.owner,
        &ModelTurn {
            run_id: &moved.run_id,
            expected_version: moved.version,
            observation_id: &stale,
            objective: OBJECTIVE,
        },
        route(),
        &action_json(&stale, "whatever", "x"),
    )
    .expect_err("stale frame");
    assert_eq!(code(&error), ComputerErrorCode::StaleObservation);
}

/// Attack: present a validly minted seal after the run has moved on.
#[tokio::test]
async fn a_sealed_proposal_cannot_apply_after_re_observation_or_cancellation() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let sealed = harness
        .accept(
            &run,
            &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
        )
        .expect("seal");
    let capabilities = harness.service.capabilities();

    let moved = harness.observe_again(&run).await;
    assert_eq!(
        code(
            &sealed
                .authorize_against(&moved, harness.owner, &capabilities, &route())
                .expect_err("stale after re-observation")
        ),
        ComputerErrorCode::Conflict
    );

    harness
        .service
        .cancel(&Uuid::new_v4().to_string(), &run.run_id)
        .await
        .expect("cancel");
    let cancelled = harness.run(&run.run_id);
    assert!(sealed
        .authorize_against(&cancelled, harness.owner, &capabilities, &route())
        .is_err());
    assert!(cancelled.last_receipt.is_none());
}

/// Attack: spend one seal as another session.
#[tokio::test]
async fn a_sealed_proposal_is_bound_to_its_run_and_owner() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let sealed = harness
        .accept(
            &run,
            &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
        )
        .expect("seal");
    let capabilities = harness.service.capabilities();
    assert_eq!(
        code(
            &sealed
                .authorize_against(&run, Uuid::new_v4(), &capabilities, &route())
                .expect_err("wrong owner")
        ),
        ComputerErrorCode::Unauthorized
    );
    assert!(sealed
        .authorize_against(&run, harness.owner, &capabilities, &route())
        .is_ok());
}

/// Two identical normalized proposals for one frame share a fingerprint but not
/// a nonce, so single-use accounting can tell two presentations apart.
#[tokio::test]
async fn duplicate_normalized_proposals_share_one_fingerprint() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let element_id = name_element_id(&run);
    let first = harness
        .accept(&run, &action_json(&frame, &element_id, TARGET_VALUE))
        .expect("first");
    let second = harness
        .accept(&run, &action_json(&frame, &element_id, TARGET_VALUE))
        .expect("second");
    assert_eq!(first.proposal_fingerprint(), second.proposal_fingerprint());
    assert_ne!(first.nonce(), second.nonce());

    let different = harness
        .accept(&run, &action_json(&frame, &element_id, "Grace Hopper"))
        .expect("other");
    assert_ne!(
        first.proposal_fingerprint(),
        different.proposal_fingerprint()
    );
}

/// Duplicate admission is durable and is consumed only after a proposal has
/// actually staged, so a refused application never burns a fingerprint.
#[tokio::test]
async fn duplicate_admission_is_durable_and_consumed_only_after_staging() {
    let dir = TempDir::new().expect("temp dir");
    let owner = Uuid::new_v4();
    let (run_id, fingerprint) = {
        let store = ComputerStore::open(dir.path()).expect("store");
        let service = Arc::new(ComputerUseService::new(
            Arc::new(FixtureBackend::new(ActMode::Positive)),
            store,
        ));
        let harness = Harness {
            _dir: TempDir::new().expect("scratch"),
            service: service.clone(),
            owner,
        };
        let run = harness.ready_run().await;
        let sealed = harness
            .accept(
                &run,
                &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
            )
            .expect("seal");
        let fingerprint = sealed.proposal_fingerprint().to_string();

        // Nothing staged yet, so nothing is admitted.
        assert!(!service.proposal_already_applied(&run.run_id, &fingerprint));
        service
            .commit_proposal_admission(&run.run_id, &fingerprint)
            .expect("admit after staging");
        assert!(service.proposal_already_applied(&run.run_id, &fingerprint));
        assert_eq!(
            code(
                &service
                    .commit_proposal_admission(&run.run_id, &fingerprint)
                    .expect_err("second admission")
            ),
            ComputerErrorCode::DuplicateProposal
        );
        (run.run_id, fingerprint)
    };

    // Durable across a process boundary: reopening the ledger keeps it.
    let reopened = ComputerStore::open(dir.path()).expect("reopen");
    let service =
        ComputerUseService::new(Arc::new(FixtureBackend::new(ActMode::Positive)), reopened);
    assert!(
        service.proposal_already_applied(&run_id, &fingerprint),
        "admission must survive a restart"
    );
}

/// Concurrent seals from one snapshot: the first to land wins, and the second
/// is refused because the run revision it is bound to has moved.
#[tokio::test]
async fn concurrent_seals_from_one_snapshot_cannot_both_apply() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let frame = current_frame(&run);
    let element_id = name_element_id(&run);
    let first = harness
        .accept(&run, &action_json(&frame, &element_id, TARGET_VALUE))
        .expect("first");
    let second = harness
        .accept(&run, &action_json(&frame, &element_id, "Grace Hopper"))
        .expect("second");
    let capabilities = harness.service.capabilities();

    first
        .authorize_against(&run, harness.owner, &capabilities, &route())
        .expect("first seal is live");
    harness
        .dispatch(&run, TARGET_VALUE)
        .await
        .expect("dispatch");

    let moved = harness.run(&run.run_id);
    assert_eq!(
        code(
            &second
                .authorize_against(&moved, harness.owner, &capabilities, &route())
                .expect_err("second seal lost the race")
        ),
        ComputerErrorCode::Conflict
    );
}

// ---------------------------------------------------------------------------
// #456 — completion proves the operator's objective, on the current frame
// ---------------------------------------------------------------------------

/// The legitimate path: dispatch, capture the one verifying frame, and complete
/// against an objective that the frame actually satisfies.
#[tokio::test]
async fn a_verified_receipt_and_a_satisfied_objective_permit_completion() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
    assert!(run.last_receipt.is_some(), "dispatch minted a receipt");

    let sealed = harness
        .accept(&run, &complete_json(&current_frame(&run)))
        .expect("seal");
    let AcceptedIntent::Complete { evidence } = sealed.intent().clone() else {
        panic!("expected a completion intent");
    };
    sealed
        .authorize_against(
            &run,
            harness.owner,
            &harness.service.capabilities(),
            &route(),
        )
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

/// The P0 case: a real dispatch with a real verifying frame proves only that
/// one action ran. If the operator's objective is not met, the run stops for a
/// person to look at — it does not complete.
#[tokio::test]
async fn a_credible_claim_with_an_unmet_objective_stops_for_review() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    // A real, approved, verified action — that types the wrong thing.
    let run = harness.dispatch_and_verify(&run, "Grace Hopper").await;
    assert!(run.last_receipt.is_some(), "the dispatch really did verify");

    let sealed = harness
        .accept(&run, &complete_json(&current_frame(&run)))
        .expect("a credible completion claim still normalizes");
    let AcceptedIntent::Complete { evidence } = sealed.intent().clone() else {
        panic!("expected a completion intent");
    };
    let error = harness
        .service
        .complete_verified(
            &Uuid::new_v4().to_string(),
            &run.run_id,
            run.version,
            &evidence,
        )
        .expect_err("the objective is not satisfied");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);
    assert!(error.message.contains("objective not satisfied"));

    let after = harness.run(&run.run_id);
    assert_ne!(after.state, ComputerRunState::Completed);
    assert_eq!(after.state, ComputerRunState::Paused);
    assert_eq!(
        after.control_disposition,
        ComputerControlDisposition::AwaitingReview
    );
    assert!(after
        .audit
        .iter()
        .any(|entry| entry.disposition == "stopped_for_review"));
}

/// A run with no operator-authored objective has no definition of success, so
/// no model turn against it can even be normalized.
#[tokio::test]
async fn a_run_without_an_authored_objective_can_never_be_completed() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run_with(None).await;
    let error = harness
        .accept(
            &run,
            &action_json(&current_frame(&run), &name_element_id(&run), TARGET_VALUE),
        )
        .expect_err("no authored objective");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);
}

/// The #456 sequence, verbatim: dispatch a positive action on frame 1, observe
/// frame 2, then propose `complete` bound to frame 2.
#[tokio::test]
async fn dispatch_then_re_observe_then_complete_is_refused() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
    assert!(run.last_receipt.is_some());

    let run = harness.observe_again(&run).await;
    assert!(
        run.last_receipt.is_none(),
        "an ordinary observation must clear completion evidence"
    );

    let error = harness
        .accept(&run, &complete_json(&current_frame(&run)))
        .expect_err("evidence did not verify this frame");
    assert_eq!(code(&error), ComputerErrorCode::UnverifiedCompletion);

    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    assert!(!after.state.is_terminal());
}

/// An action whose effect a semantic frame cannot show can never carry a
/// completion proof. "We could not check" must not stand in for "it worked".
#[tokio::test]
async fn an_opaque_action_can_never_prove_completion() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    harness
        .service
        .act(
            &Uuid::new_v4().to_string(),
            &run.run_id,
            run.version,
            &current_frame(&run),
            ComputerAction::ActivateTarget,
        )
        .await
        .expect("activate");
    let acted = harness.run(&run.run_id);
    harness
        .service
        .observe_postcondition(&Uuid::new_v4().to_string(), &run.run_id, acted.version)
        .await
        .expect("postcondition frame");
    let after = harness.run(&run.run_id);
    assert!(
        after.last_receipt.is_none(),
        "an opaque expectation must not become evidence"
    );
    assert_eq!(
        code(
            &harness
                .accept(&after, &complete_json(&current_frame(&after)))
                .expect_err("opaque action")
        ),
        ComputerErrorCode::UnverifiedCompletion
    );
}

/// A dispatch the backend does not confirm can never become evidence.
#[tokio::test]
async fn a_non_positive_outcome_never_becomes_completion_evidence() {
    let harness = Harness::new(ActMode::Unconfirmed);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
    assert!(run.last_receipt.is_none());
    assert_eq!(
        code(
            &harness
                .accept(&run, &complete_json(&current_frame(&run)))
                .expect_err("no positive evidence")
        ),
        ComputerErrorCode::UnverifiedCompletion
    );
}

/// Attack: forge evidence field by field.
#[tokio::test]
async fn mismatched_receipt_fingerprint_frame_or_revision_is_refused() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
    let sealed = harness
        .accept(&run, &complete_json(&current_frame(&run)))
        .expect("seal");
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
        assert_eq!(harness.run(&run.run_id).state, ComputerRunState::Ready);
    }
}

/// Attack: capture evidence while valid, spend it one frame later.
#[tokio::test]
async fn captured_evidence_cannot_be_replayed_onto_a_later_frame() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
    let sealed = harness
        .accept(&run, &complete_json(&current_frame(&run)))
        .expect("seal");
    let AcceptedIntent::Complete { evidence } = sealed.intent().clone() else {
        panic!("expected a completion intent");
    };

    let moved = harness.observe_again(&run).await;
    assert_eq!(
        code(
            &harness
                .service
                .complete_verified(
                    &Uuid::new_v4().to_string(),
                    &moved.run_id,
                    moved.version,
                    &evidence,
                )
                .expect_err("replayed evidence")
        ),
        ComputerErrorCode::UnverifiedCompletion
    );
    assert_eq!(harness.run(&run.run_id).state, ComputerRunState::Ready);
}

/// Steering, takeover, and pause each move the authority revision and strand
/// the evidence bound to the previous one.
#[tokio::test]
async fn authority_changes_clear_completion_evidence() {
    for label in ["pause", "take_over"] {
        let harness = Harness::new(ActMode::Positive);
        let run = harness.ready_run().await;
        let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
        assert!(
            run.last_receipt.is_some(),
            "{label}: dispatch minted evidence"
        );

        if label == "take_over" {
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

// ---------------------------------------------------------------------------
// Crash cuts and post-effect uncertainty
// ---------------------------------------------------------------------------

/// Once the backend has been asked to act, the host cannot know whether the
/// machine was touched. Every failure from that point is uncertain and needs
/// operator reconciliation — including one the backend claims was clean.
#[tokio::test]
async fn post_effect_failures_are_classified_uncertain() {
    for mode in [ActMode::FailBeforeEffect, ActMode::FailAfterEffect] {
        let harness = Harness::new(mode);
        let run = harness.ready_run().await;
        let error = harness
            .dispatch(&run, TARGET_VALUE)
            .await
            .expect_err("backend failed");
        assert_eq!(
            code(&error),
            ComputerErrorCode::UncertainOutcome,
            "{mode:?} must be uncertain, not a clean failure"
        );
        let after = harness.run(&run.run_id);
        assert_eq!(
            after.control_disposition,
            ComputerControlDisposition::UncertainOutcome
        );
        assert!(after.last_receipt.is_none());
        assert!(after.current_observation.is_none());
        assert!(
            after.last_outcome.is_none(),
            "an in-flight failure must not leave a positive outcome standing"
        );
    }

    // The exception is deliberate and narrow: an adapter checks permission
    // before it acts, so an authorization refusal is a positive statement that
    // nothing was touched — and the operator needs to see *that*, not a
    // generic "uncertain", to know a re-grant is what unblocks them.
    let harness = Harness::new(ActMode::RefuseUnauthorized);
    let run = harness.ready_run().await;
    let error = harness
        .dispatch(&run, TARGET_VALUE)
        .await
        .expect_err("permission refused");
    assert_eq!(code(&error), ComputerErrorCode::PermissionRevoked);
    assert_ne!(
        harness.run(&run.run_id).control_disposition,
        ComputerControlDisposition::UncertainOutcome
    );
}

/// Crash cuts. Each stops the process at a different point around the effect,
/// and every one of them must recover to a state that cannot complete.
///
/// A restart is modelled the way the ledger sees one: the store is reopened,
/// which runs interrupted-run recovery.
#[tokio::test]
async fn crash_cuts_around_the_effect_all_recover_fail_closed() {
    #[derive(Debug, Clone, Copy)]
    enum Cut {
        BeforeMutation,
        AfterMutationBeforeReceipt,
        AfterReceiptBeforePostcondition,
        DuringPostconditionCapture,
    }

    for cut in [
        Cut::BeforeMutation,
        Cut::AfterMutationBeforeReceipt,
        Cut::AfterReceiptBeforePostcondition,
        Cut::DuringPostconditionCapture,
    ] {
        let dir = TempDir::new().expect("temp dir");
        let owner = Uuid::new_v4();
        let run_id = {
            let mode = match cut {
                Cut::BeforeMutation => ActMode::FailBeforeEffect,
                Cut::AfterMutationBeforeReceipt => ActMode::FailAfterEffect,
                _ => ActMode::Positive,
            };
            let backend = Arc::new(FixtureBackend::new(mode));
            let store = ComputerStore::open(dir.path()).expect("store");
            let service = Arc::new(ComputerUseService::new(backend.clone(), store));
            let harness = Harness {
                _dir: TempDir::new().expect("scratch"),
                service: service.clone(),
                owner,
            };
            let run = harness.ready_run().await;
            let _ = harness.dispatch(&run, TARGET_VALUE).await;

            if matches!(cut, Cut::DuringPostconditionCapture) {
                *backend.fail_observe.lock() = true;
                let acted = harness.run(&run.run_id);
                let _ = service
                    .observe_postcondition(&Uuid::new_v4().to_string(), &run.run_id, acted.version)
                    .await;
            }
            // `Cut::AfterReceiptBeforePostcondition` simply never captures the
            // verifying frame before the process stops.
            run.run_id
        };

        let reopened = ComputerStore::open(dir.path()).expect("reopen store");
        let service =
            ComputerUseService::new(Arc::new(FixtureBackend::new(ActMode::Positive)), reopened);
        let recovered = service.get_run(&run_id).expect("load").expect("run exists");
        assert!(
            recovered.state.is_terminal(),
            "{cut:?}: a cut run must not resume as if nothing happened"
        );
        assert!(
            recovered.last_receipt.is_none(),
            "{cut:?}: no completion evidence may survive a cut"
        );
        assert!(recovered.last_outcome.is_none(), "{cut:?}");
        assert!(recovered.current_observation.is_none(), "{cut:?}");
        // Authority must be unusable. A run that reached a terminal state
        // *before* the restart keeps its revoked grant record — recovery only
        // rewrites runs that were still live — so the invariant is "absent or
        // revoked", not "absent".
        assert!(
            recovered
                .grant
                .as_ref()
                .is_none_or(|grant| grant.revoked_at.is_some()),
            "{cut:?}: authority survived a cut"
        );

        // And nothing can be proposed against the recovered run.
        assert!(accept_model_output(
            &service,
            owner,
            &ModelTurn {
                run_id: &run_id,
                expected_version: recovered.version,
                observation_id: "any-observation",
                objective: OBJECTIVE,
            },
            route(),
            &complete_json("any-observation"),
        )
        .is_err());
    }
}

/// A restart strands every run and its evidence, and the recovered record is
/// isolated from the session that created it.
#[tokio::test]
async fn restart_recovery_strands_evidence_and_authority() {
    let dir = TempDir::new().expect("temp dir");
    let owner = Uuid::new_v4();
    let run_id = {
        let store = ComputerStore::open(dir.path()).expect("store");
        let service = Arc::new(ComputerUseService::new(
            Arc::new(FixtureBackend::new(ActMode::Positive)),
            store,
        ));
        let harness = Harness {
            _dir: TempDir::new().expect("scratch"),
            service: service.clone(),
            owner,
        };
        let run = harness.ready_run().await;
        let run = harness.dispatch_and_verify(&run, TARGET_VALUE).await;
        assert!(run.last_receipt.is_some());
        run.run_id
    };

    let reopened = ComputerStore::open(dir.path()).expect("reopen store");
    let service =
        ComputerUseService::new(Arc::new(FixtureBackend::new(ActMode::Positive)), reopened);
    let recovered = service.get_run(&run_id).expect("load").expect("run exists");
    assert_eq!(recovered.state, ComputerRunState::Interrupted);
    assert!(recovered.last_receipt.is_none());
    assert!(recovered.last_outcome.is_none());
    assert!(recovered.current_observation.is_none());
}

/// Typed refusals reach operators through the run's existing durable event
/// journal, and each one advances the run revision so the refused capability —
/// and any sibling minted from the same snapshot — is immediately dead.
#[tokio::test]
async fn refusals_are_journaled_and_advance_the_run_revision() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let error = harness
        .accept(&run, &complete_json(&current_frame(&run)))
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

    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    assert!(
        after.version > run.version,
        "a refusal must advance the revision so the refused seal cannot be retried"
    );
}

/// The public projection is what a coordinator and the cockpit both read. It
/// must carry no model-authored text, no observed content, and no host paths.
#[tokio::test]
async fn the_public_projection_carries_no_needles() {
    let harness = Harness::new(ActMode::Positive);
    let run = harness.ready_run().await;
    let run = harness
        .dispatch_and_verify(&run, "Authorization: Bearer sk-live-secret")
        .await;

    let projection = harness
        .service
        .project_session_run(harness.owner, &run.run_id, Utc::now())
        .expect("projection");
    let serialized = serde_json::to_string(&projection).expect("serialize");
    for needle in [
        "sk-live-secret",
        "Bearer",
        OBJECTIVE,
        "/Users/",
        "asset_id",
        "content_sha256",
    ] {
        assert!(
            !serialized.contains(needle),
            "projection leaked {needle:?}: {serialized}"
        );
    }
    // The observation identity is projected through a run-scoped surrogate, so
    // even the internal frame id does not appear verbatim.
    assert!(!serialized.contains(&current_frame(&run)));
}
