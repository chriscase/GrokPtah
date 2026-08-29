//! The full path, end to end, through adaptive admission (#435).
//!
//! Every other suite in this lane proves one stage in isolation:
//! `computer_use_sealed_boundary` proves the kernel seal, and
//! `computer_use_adaptive_durability` proves admission, revalidation, and
//! durability. Neither walks the whole thing, and a layer that is correct at
//! every stage can still be wrong at a seam.
//!
//! So this suite walks it, in order and without a helper that hides a stage:
//!
//! ```text
//! host admission -> provider -> seal -> profile budget
//!                -> operator approval -> dispatch -> postcondition -> completion
//! ```
//!
//! Two properties matter more than the happy path, and every gate here asserts
//! both:
//!
//! - **which stage refused.** A test that only checks "this failed" cannot tell
//!   a budget rejection from a seal rejection, and the difference is the whole
//!   safety argument: the seal must refuse first, and the profile must only
//!   ever be able to refuse *more*.
//! - **zero dispatches on any refusal.** The backend counts every `act` it is
//!   asked to perform. A refusal at any stage before dispatch must leave that
//!   counter where it was, so nothing reaches the operating system.
//!
//! No provider is called: the "provider" is a fixed byte string per turn, which
//! is exactly the authority a real provider response carries — none.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use tempfile::TempDir;
use uuid::Uuid;

use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities,
    ComputerErrorCode, ComputerObservation, ComputerResult, ComputerRun, ComputerRunState,
    ComputerStore, ComputerTarget, ComputerUseLimits, GrantIssuer, ObservationGeometry,
    ReceiptVerification, SemanticAction, SemanticElement, Sensitivity,
};
use grokptah_agent_bridge::{
    accept_model_proposal, enforce_profile_budget, render_computer_observation, AcceptedIntent,
    AdaptiveAttemptOutcome, AdaptiveLifecycle, AdaptiveProfile, CapabilityEvidence,
    CapabilitySource, ComputerUseService, ComputerUseTier, HostCapabilityEvidence,
    ModelCapabilities, ModelCapabilityEvidence, ModelProposalContext, OperatorCapabilityPolicy,
    ProfileReason, RawModelProposal, TaskRisk,
};

const NAME_ELEMENT: &str = "name-field";

/// What the stand-in provider "reports" as usage on any turn whose response
/// arrived. Fixed so the gates can assert an exact running total.
const REPORTED_USAGE: (Option<u64>, Option<u64>) = (Some(11), Some(7));

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Deterministic backend that really does dispatch, and counts it.
///
/// The counter is the load-bearing part: it is the only way a test can prove
/// that a refusal happened *before* the operating system was touched, rather
/// than after.
#[derive(Debug, Default)]
struct FixtureBackend {
    sequence: AtomicU64,
    dispatches: AtomicU64,
    value: parking_lot::Mutex<Option<String>>,
}

impl FixtureBackend {
    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.example.end-to-end".into(),
            window_id: "window-1".into(),
            generation: 1,
            display_name: "End To End".into(),
            sensitivity: Sensitivity::None,
        }
    }
}

#[async_trait]
impl ComputerBackend for FixtureBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "adaptive_end_to_end_fixture".into(),
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
            elements: vec![SemanticElement {
                element_id: NAME_ELEMENT.into(),
                role: "text_field".into(),
                label: Some("Name".into()),
                value: self.value.lock().clone(),
                bounds: None,
                enabled: true,
                focused: true,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }],
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
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        if let ComputerAction::SetValue { text, .. } = action {
            *self.value.lock() = Some(text.clone());
        }
        Ok(ActionOutcome::bounded("fixture action", Some(true)))
    }

    async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
        Ok(())
    }
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        tools: true,
        image_input: false,
        max_image_bytes: None,
        computer_use_tier: ComputerUseTier::SemanticAct,
        computer_capability_source: CapabilitySource::Measured,
        ..Default::default()
    }
}

fn evidence(route: &str, credential: &str) -> CapabilityEvidence {
    CapabilityEvidence::new(
        ModelCapabilityEvidence::from_model_capabilities(
            &capabilities(),
            true,
            false,
            route,
            credential,
            &OperatorCapabilityPolicy::default(),
        ),
        HostCapabilityEvidence {
            semantic_observation: true,
            screenshot_capture: false,
            // This build has no verifier independent of the proposing model.
            independent_verifier: false,
        },
    )
}

struct Harness {
    _dir: TempDir,
    backend: Arc<FixtureBackend>,
    service: Arc<ComputerUseService>,
    owner: Uuid,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = ComputerStore::open(dir.path()).expect("store");
        let backend = Arc::new(FixtureBackend::default());
        Self {
            _dir: dir,
            service: Arc::new(ComputerUseService::new(backend.clone(), store)),
            backend,
            owner: Uuid::new_v4(),
        }
    }

    fn dispatches(&self) -> u64 {
        self.backend.dispatches.load(Ordering::SeqCst)
    }

    fn run(&self, run_id: &str) -> ComputerRun {
        self.service
            .get_run(run_id)
            .expect("load run")
            .expect("run exists")
    }

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
}

// ---------------------------------------------------------------------------
// The path itself
// ---------------------------------------------------------------------------

/// Every stage a turn passes through, in order. A refusal names the stage that
/// produced it, which is what makes "the seal refused, not the budget" a thing
/// a test can assert rather than a thing a comment claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Admission,
    Provider,
    Seal,
    Budget,
    Approval,
    Dispatch,
    Postcondition,
    Completion,
}

#[derive(Debug)]
struct Refused {
    stage: Stage,
    code: ComputerErrorCode,
}

/// What the operator did when the host staged an action for approval. A sealed
/// proposal is never a dispatch: the operator still has to say yes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Approval {
    Approve,
    Decline,
}

/// One complete turn, written out stage by stage on purpose.
///
/// A helper that collapsed these into `run_turn()` would make the suite shorter
/// and would also make it useless: the ordering *is* the property under test.
async fn turn(
    harness: &Harness,
    run_id: &str,
    risk: TaskRisk,
    approval: Approval,
    provider_reply: impl FnOnce(&ComputerObservation) -> Result<RawModelProposal, ComputerErrorCode>,
) -> Result<Stage, Refused> {
    // --- host: admission -------------------------------------------------
    // Authority is re-derived here, every turn, from the live record: revision
    // CAS, capability generation, and task risk against the run's high-water
    // mark. Nothing is carried over from the previous turn.
    let permit = harness
        .service
        .begin_adaptive_turn(
            run_id,
            harness.owner,
            &evidence("route/end-to-end", "credential-1"),
            risk,
        )
        .map_err(|error| Refused {
            stage: Stage::Admission,
            code: error.code,
        })?;

    let run = harness.run(run_id);
    let observation = run
        .current_observation
        .clone()
        .expect("a ready run has a current observation");

    // --- provider --------------------------------------------------------
    // The attempt is counted *before* the bytes are asked for, so a turn that
    // fails anywhere downstream still costs what it cost.
    harness
        .service
        .record_adaptive_attempt(run_id)
        .expect("count the attempt");
    let (_rendered_json, rendered) = render_computer_observation(&observation, permit.profile);
    assert_eq!(
        rendered.actionable_elements, 1,
        "the profile's view must offer exactly the one actionable element"
    );

    // Closes a turn that failed, and returns the refusal. `usage` is recorded
    // here too: a response that arrived and then failed to parse was still
    // billed, so dropping its tokens would understate what the run spent. A
    // transport failure that never produced a response reports no usage, which
    // is a different thing from reporting zero.
    let fail = |stage: Stage, code: ComputerErrorCode, usage: (Option<u64>, Option<u64>)| {
        harness
            .service
            .finish_adaptive_turn(
                run_id,
                usage,
                AdaptiveAttemptOutcome::Failed {
                    observation_bytes: rendered.bytes,
                },
            )
            .expect("account for the failed turn");
        Refused { stage, code }
    };

    // The attempt above is already spent, so a transport failure, a timeout, or
    // a body that never arrives costs the run exactly what a success costs it.
    let raw =
        provider_reply(&observation).map_err(|code| fail(Stage::Provider, code, (None, None)))?;

    // --- seal ------------------------------------------------------------
    // The single universal validator, run against the live record. Raw provider
    // bytes carry no authority until this returns.
    let context =
        ModelProposalContext::from_run(&run, harness.owner, harness.service.capabilities())
            .map_err(|error| fail(Stage::Seal, error.code, REPORTED_USAGE))?;
    let accepted = accept_model_proposal(&context, &raw)
        .map_err(|error| fail(Stage::Seal, error.code, REPORTED_USAGE))?;

    // --- profile budget --------------------------------------------------
    // Strictly after the seal, and able only to reject more.
    enforce_profile_budget(&accepted, permit.profile)
        .map_err(|error| fail(Stage::Budget, error.code, REPORTED_USAGE))?;

    match accepted.intent() {
        AcceptedIntent::Complete { evidence } => {
            let evidence = evidence.clone();
            accepted
                .authorize_against(&run, harness.owner)
                .map_err(|error| fail(Stage::Approval, error.code, REPORTED_USAGE))?;
            harness
                .service
                .complete_verified(&Uuid::new_v4().to_string(), run_id, run.version, &evidence)
                .map_err(|error| fail(Stage::Completion, error.code, REPORTED_USAGE))?;
            harness
                .service
                .finish_adaptive_turn(
                    run_id,
                    REPORTED_USAGE,
                    AdaptiveAttemptOutcome::Succeeded {
                        observation_bytes: rendered.bytes,
                        truncated: rendered.truncated,
                    },
                )
                .expect("account for the turn");
            harness.service.record_adaptive_completed(run_id);
            Ok(Stage::Completion)
        }
        AcceptedIntent::Action { action, .. } => {
            let action = action.clone();

            // --- operator approval ---------------------------------------
            // Staging is not dispatch. The sealed evidence is re-checked
            // against the live run, and then a human still has to say yes.
            accepted
                .authorize_against(&run, harness.owner)
                .map_err(|error| fail(Stage::Approval, error.code, REPORTED_USAGE))?;
            if approval == Approval::Decline {
                return Err(fail(
                    Stage::Approval,
                    ComputerErrorCode::Unauthorized,
                    REPORTED_USAGE,
                ));
            }

            // --- dispatch ------------------------------------------------
            // The kernel revalidates everything again here; the seal did not
            // buy the caller a shortcut past `authorize_action`.
            harness
                .service
                .act(
                    &Uuid::new_v4().to_string(),
                    run_id,
                    run.version,
                    &observation.observation_id,
                    action,
                )
                .await
                .map_err(|error| fail(Stage::Dispatch, error.code, REPORTED_USAGE))?;

            // --- postcondition -------------------------------------------
            // One host-issued verifying frame, captured by the host and not by
            // anything the model said.
            let acted = harness.run(run_id);
            harness
                .service
                .observe_postcondition(&Uuid::new_v4().to_string(), run_id, acted.version)
                .await
                .map_err(|error| fail(Stage::Postcondition, error.code, REPORTED_USAGE))?;

            harness
                .service
                .finish_adaptive_turn(
                    run_id,
                    REPORTED_USAGE,
                    AdaptiveAttemptOutcome::Succeeded {
                        observation_bytes: rendered.bytes,
                        truncated: rendered.truncated,
                    },
                )
                .expect("account for the turn");
            Ok(Stage::Postcondition)
        }
    }
}

fn set_value(observation: &ComputerObservation, text: &str) -> RawModelProposal {
    RawModelProposal::new(
        serde_json::json!({
            "observation_id": observation.observation_id,
            "action_type": "set_value",
            "element_id": NAME_ELEMENT,
            "text": text,
            "summary": "Enter the visible name"
        })
        .to_string(),
    )
}

fn complete(observation: &ComputerObservation) -> RawModelProposal {
    RawModelProposal::new(
        serde_json::json!({
            "observation_id": observation.observation_id,
            "action_type": "complete",
            "summary": "The visible objective is satisfied"
        })
        .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// **The whole path.** Admission through verified completion, with the durable
/// record and the operator projection both telling the same story afterwards.
#[tokio::test]
async fn the_full_path_runs_end_to_end_and_dispatches_exactly_once() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    let reached = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |observation| Ok(set_value(observation, "Ada Lovelace")),
    )
    .await
    .expect("the action turn completes");
    assert_eq!(reached, Stage::Postcondition);
    assert_eq!(harness.dispatches(), 1, "exactly one dispatch");

    // The host minted and verified a receipt on the single postcondition frame.
    let acted = harness.run(&run.run_id);
    let receipt = acted
        .last_receipt
        .as_ref()
        .expect("dispatch minted a receipt");
    assert!(
        matches!(receipt.verification, ReceiptVerification::Verified { .. }),
        "the postcondition frame verified the receipt: {:?}",
        receipt.verification
    );

    // Completion is a second turn, and it goes through the same admission.
    let reached = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |observation| Ok(complete(observation)),
    )
    .await
    .expect("the completion turn completes");
    assert_eq!(reached, Stage::Completion);
    assert_eq!(harness.dispatches(), 1, "completion dispatches nothing");

    let finished = harness.run(&run.run_id);
    assert_eq!(finished.state, ComputerRunState::Completed);
    assert!(
        finished
            .grant
            .as_ref()
            .is_some_and(|grant| grant.revoked_at.is_some()),
        "completion revokes authority"
    );

    // The durable record and the shared projection agree with the kernel.
    let record = finished
        .adaptive
        .as_ref()
        .expect("adaptive record survived");
    assert_eq!(record.lifecycle, AdaptiveLifecycle::Completed);
    assert_eq!(record.profile, AdaptiveProfile::Economy);
    assert_eq!(record.cost.provider_attempts, 2);
    assert_eq!(record.cost.accepted_attempts, 2);
    assert_eq!(record.cost.failed_attempts, 0);
    assert_eq!(
        record.cost.screenshot_bytes, 0,
        "no pixels ever reach a model"
    );
    assert_eq!(record.cost.prompt_tokens, Some(22));
    assert_eq!(record.cost.completion_tokens, Some(14));

    let projection = harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .expect("a record exists");
    assert_eq!(projection.lifecycle, AdaptiveLifecycle::Completed);
    assert_eq!(projection.cost.provider_attempts, 2);
}

/// **A forged completion never reaches the operating system.** The provider
/// claims the objective is done on a run that has dispatched nothing, so no
/// receipt exists. The seal — not the budget, not the kernel's `act` — must be
/// the stage that refuses.
#[tokio::test]
async fn a_forged_completion_is_refused_at_the_seal_with_zero_dispatches() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    let refused = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |observation| Ok(complete(observation)),
    )
    .await
    .expect_err("a completion with no host evidence must fail closed");

    assert_eq!(refused.stage, Stage::Seal);
    assert_eq!(refused.code, ComputerErrorCode::UnverifiedCompletion);
    assert_eq!(harness.dispatches(), 0, "nothing reached the backend");

    // The run is still usable, and the failed turn was still paid for.
    let after = harness.run(&run.run_id);
    assert_eq!(after.state, ComputerRunState::Ready);
    let record = after.adaptive.as_ref().expect("record");
    assert_eq!(record.cost.provider_attempts, 1);
    assert_eq!(record.cost.failed_attempts, 1);
    assert_eq!(record.cost.accepted_attempts, 0);
    assert_eq!(
        record.cost.prompt_tokens,
        Some(11),
        "usage survives a refused turn"
    );
}

/// **The profile can only reject more, and it rejects after the seal.** The
/// same proposal that the seal accepts is refused by the Economy text ceiling,
/// and the refusal still lands before dispatch.
#[tokio::test]
async fn a_profile_budget_refusal_lands_after_the_seal_and_before_dispatch() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    let ceiling = AdaptiveProfile::Economy.budget().max_text_entry_bytes as usize;
    let oversized = "A".repeat(ceiling + 1);

    let refused = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |observation| Ok(set_value(observation, &oversized)),
    )
    .await
    .expect_err("the Economy text ceiling refuses this");

    assert_eq!(
        refused.stage,
        Stage::Budget,
        "the seal accepted it; only the profile rejected it"
    );
    assert_eq!(refused.code, ComputerErrorCode::InvalidRequest);
    assert_eq!(harness.dispatches(), 0, "nothing reached the backend");

    // The kernel would have accepted this text, which is the point: the profile
    // is strictly narrower than the safety floor, never wider.
    assert!(
        oversized.len() <= ComputerUseLimits::ceiling().max_text_entry_bytes as usize,
        "the oversized text is still inside the kernel ceiling, so only the \
         profile can be what refused it"
    );
}

/// **A staged action is not a dispatch.** The seal accepted it, the budget
/// accepted it, and the operator declined. Nothing reaches the backend, and the
/// run stays usable.
#[tokio::test]
async fn a_declined_approval_never_dispatches() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    let refused = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Decline,
        |observation| Ok(set_value(observation, "Ada Lovelace")),
    )
    .await
    .expect_err("a declined action does not proceed");

    assert_eq!(refused.stage, Stage::Approval);
    assert_eq!(harness.dispatches(), 0, "nothing reached the backend");
    assert_eq!(harness.run(&run.run_id).state, ComputerRunState::Ready);
}

/// **A later higher-risk objective stops the run at admission.** The turn never
/// reaches the provider, so it costs nothing, and it certainly never dispatches.
#[tokio::test]
async fn a_higher_risk_objective_stops_at_admission_before_any_spend() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |observation| Ok(set_value(observation, "Ada Lovelace")),
    )
    .await
    .expect("the routine turn completes");
    let spend_before = harness
        .run(&run.run_id)
        .adaptive
        .expect("record")
        .cost
        .provider_attempts;

    // The operator now asks for something destructive on the same run.
    let refused = turn(
        &harness,
        &run.run_id,
        TaskRisk::Destructive,
        Approval::Approve,
        |observation| Ok(set_value(observation, "anything")),
    )
    .await
    .expect_err("a higher-risk objective must not reuse occupied state");

    assert_eq!(refused.stage, Stage::Admission);
    assert_eq!(refused.code, ComputerErrorCode::Unauthorized);
    assert_eq!(
        harness.dispatches(),
        1,
        "still only the first turn's dispatch"
    );

    let record = harness.run(&run.run_id).adaptive.expect("record");
    assert_eq!(
        record.cost.provider_attempts, spend_before,
        "a turn refused at admission never reached a provider, so it cost nothing"
    );
    assert_eq!(record.lifecycle, AdaptiveLifecycle::Stopped);
    assert!(
        record.terminal.is_some(),
        "the stop is durable, with a reason an operator can read"
    );
}

/// **A stopped run stays stopped.** Once admission has stopped the run, every
/// later turn is refused at admission too — a stop is not a one-turn veto.
#[tokio::test]
async fn a_stopped_run_admits_nothing_afterwards() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    turn(
        &harness,
        &run.run_id,
        TaskRisk::Destructive,
        Approval::Approve,
        |observation| Ok(set_value(observation, "anything")),
    )
    .await
    .expect_err("no independent verifier exists, so a destructive run stops");

    // The stop is durable and legible. Before this was written as a record, a
    // selection-time refusal produced no projection at all, so the operator saw
    // an untouched run and no reason.
    let projection = harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .expect("a refused run still has an adaptive record");
    assert_eq!(projection.lifecycle, AdaptiveLifecycle::Stopped);
    let terminal = projection
        .terminal
        .as_ref()
        .expect("the stop names its reason");
    assert_eq!(
        terminal.reason,
        ProfileReason::IndependentVerifierUnavailable
    );
    assert!(
        !terminal.message.is_empty(),
        "the reason renders as a sentence an operator can act on"
    );
    assert_eq!(
        projection.cost.provider_attempts, 0,
        "a run refused at selection never called a provider"
    );

    for _ in 0..3 {
        let refused = turn(
            &harness,
            &run.run_id,
            TaskRisk::Routine,
            Approval::Approve,
            |observation| Ok(set_value(observation, "Ada Lovelace")),
        )
        .await
        .expect_err("a stopped run does not quietly resume at a lower risk");
        assert_eq!(refused.stage, Stage::Admission);
    }
    assert_eq!(harness.dispatches(), 0, "nothing ever reached the backend");
}

/// **A provider failure costs what a success costs.** No body ever arrives, so
/// there is nothing to seal and nothing to dispatch — but the attempt was made,
/// and the run is charged for it. A layer that only counted successful calls
/// would let a run loop on timeouts forever inside a budget it never spent.
#[tokio::test]
async fn a_provider_transport_failure_is_still_a_paid_attempt() {
    let harness = Harness::new();
    let run = harness.ready_run().await;

    let refused = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |_observation| Err(ComputerErrorCode::Interrupted),
    )
    .await
    .expect_err("the provider never answered");

    assert_eq!(refused.stage, Stage::Provider);
    assert_eq!(harness.dispatches(), 0, "nothing reached the backend");

    let record = harness.run(&run.run_id).adaptive.expect("record");
    assert_eq!(
        record.cost.provider_attempts, 1,
        "the attempt is counted before the request leaves the host"
    );
    assert_eq!(record.cost.failed_attempts, 1);
    assert_eq!(record.cost.accepted_attempts, 0);
    assert_eq!(
        record.cost.prompt_tokens, None,
        "a response that never arrived reported no usage, which is not zero usage"
    );

    // The run is not poisoned by one failure: the next turn still runs.
    let reached = turn(
        &harness,
        &run.run_id,
        TaskRisk::Routine,
        Approval::Approve,
        |observation| Ok(set_value(observation, "Ada Lovelace")),
    )
    .await
    .expect("a retry after a transport failure is admitted");
    assert_eq!(reached, Stage::Postcondition);
    assert_eq!(harness.dispatches(), 1);

    let record = harness.run(&run.run_id).adaptive.expect("record");
    assert_eq!(
        record.cost.provider_attempts, 2,
        "both attempts are counted"
    );
    assert_eq!(
        record.cost.provider_attempts,
        record.cost.accepted_attempts + record.cost.failed_attempts,
        "attempts always reconcile"
    );
}
