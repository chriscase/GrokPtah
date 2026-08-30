//! The adaptive seam narrows the admission path and can never widen it.
//!
//! These tests drive the real [`ComputerUseService`] against a deterministic
//! in-process backend. Nothing here opens an application, requests a macOS
//! permission, dispatches OS input, launches a VM, or calls a provider.
//!
//! The properties under test are the ones that would matter if a cheap local
//! model were driving:
//!
//! * the plain `act` path is untouched, so nothing that exists today changes;
//! * a planner claim can only cost an action its admission, never buy one;
//! * uncertain work is refused *before* the backend is reached, and repeating
//!   it never quietly dispatches;
//! * the decision that was reached is projected in redacted form.
//!
//! Backend dispatch counts are asserted directly rather than inferred from the
//! returned error, because "was it refused" and "did it happen anyway" are
//! different questions and only the second one matters for safety.

use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use tempfile::TempDir;
use uuid::Uuid;

use grokptah_agent_bridge::computer_use::{
    project_run_at, ActionClass, ActionGrant, ActionOutcome, AdaptiveClaim, AdaptiveDisposition,
    AdaptiveProfile, AdaptiveReason, AmbiguityAssessment, ComputerAction, ComputerBackend,
    ComputerCapabilities, ComputerError, ComputerErrorCode, ComputerObservation, ComputerRun,
    ComputerStore, ComputerTarget, ComputerUseLimits, GrantIssuer, ObservationGeometry,
    SemanticAction, SemanticElement, Sensitivity,
};
use grokptah_agent_bridge::ComputerUseService;

const ELEMENT_ID: &str = "primary-button";
const ELEMENT_LABEL: &str = "Confidential Q3 Results";
const ELEMENT_ROLE: &str = "button";

/// A benign, deterministic backend that counts how many times it was actually
/// asked to act.
#[derive(Debug, Default)]
struct SeamBackend {
    action_calls: AtomicUsize,
    observe_calls: AtomicUsize,
}

impl SeamBackend {
    fn action_calls(&self) -> usize {
        self.action_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ComputerBackend for SeamBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "adaptive_seam_fixture".into(),
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
    ) -> Result<ComputerObservation, ComputerError> {
        let sequence = self.observe_calls.fetch_add(1, Ordering::SeqCst) as u64 + 1;
        Ok(ComputerObservation {
            // Host-minted identity: the backend echoes it rather than choosing.
            observation_id: observation_id.to_string(),
            sequence,
            target: target.clone(),
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 1024.0,
                height: 768.0,
                scale_factor: 2.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: ELEMENT_ID.into(),
                role: ELEMENT_ROLE.into(),
                label: Some(ELEMENT_LABEL.into()),
                value: None,
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::Invoke]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        })
    }

    async fn act(
        &self,
        _run_id: &str,
        _observation: &ComputerObservation,
        _action: &ComputerAction,
    ) -> Result<ActionOutcome, ComputerError> {
        self.action_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutcome::bounded("fixture acted", Some(true)))
    }

    async fn cancel(&self, _run_id: &str) -> Result<(), ComputerError> {
        Ok(())
    }
}

fn target() -> ComputerTarget {
    ComputerTarget {
        app_id: "com.grokptah.adaptive-seam-fixture".into(),
        window_id: "main-window".into(),
        generation: 1,
        display_name: "Disposable adaptive-seam fixture".into(),
        sensitivity: Sensitivity::None,
    }
}

fn grant(run: &ComputerRun, classes: BTreeSet<ActionClass>) -> ActionGrant {
    let now = Utc::now();
    ActionGrant {
        grant_id: "adaptive-seam-grant".into(),
        run_id: run.run_id.clone(),
        target: run.target.clone(),
        action_classes: classes,
        issued_by: GrantIssuer::LocalUser,
        issued_at: now - Duration::seconds(1),
        expires_at: now + Duration::minutes(5),
        uses_remaining: None,
        revoked_at: None,
    }
}

fn fixture(
    classes: BTreeSet<ActionClass>,
) -> (TempDir, Arc<SeamBackend>, ComputerUseService, ComputerRun) {
    let directory = tempfile::tempdir().expect("fixture directory");
    let backend = Arc::new(SeamBackend::default());
    let store = ComputerStore::open(directory.path().join("computer-use")).expect("store");
    let service = ComputerUseService::new(backend.clone(), store);
    let run = service
        .create_run(
            "adaptive-seam-create",
            Uuid::new_v4(),
            None,
            target(),
            Default::default(),
        )
        .expect("create run");
    let run = service
        .authorize(
            "adaptive-seam-authorize",
            &run.run_id,
            run.version,
            grant(&run, classes),
        )
        .expect("authorize run");
    (directory, backend, service, run)
}

fn semantic() -> BTreeSet<ActionClass> {
    BTreeSet::from([ActionClass::Semantic])
}

fn invoke() -> ComputerAction {
    ComputerAction::Invoke {
        element_id: ELEMENT_ID.into(),
    }
}

/// A claim that agrees with a healthy run.
fn claim(profile: AdaptiveProfile, run: &ComputerRun, sequence: u64) -> AdaptiveClaim {
    AdaptiveClaim {
        profile,
        planner: AdaptiveDisposition::Commit,
        assessment: AmbiguityAssessment::unambiguous(9_600),
        observed_control_epoch: run.control_epoch,
        observed_sequence: sequence,
        approval: None,
    }
}

/// Observe once and return the refreshed run plus the observation sequence.
async fn observed(
    service: &ComputerUseService,
    run: &ComputerRun,
    request_id: &str,
) -> (ComputerRun, u64) {
    let observation = service
        .observe(request_id, &run.run_id, run.version)
        .await
        .expect("observation");
    let refreshed = service
        .get_run(&run.run_id)
        .expect("run readable")
        .expect("run exists");
    (refreshed, observation.sequence)
}

fn adaptive_of(
    service: &ComputerUseService,
    run_id: &str,
) -> Option<grokptah_agent_bridge::computer_use::AdaptiveDecisionSummary> {
    let run = service
        .get_run(run_id)
        .expect("run readable")
        .expect("run exists");
    project_run_at(&run, Utc::now()).adaptive
}

#[tokio::test]
async fn the_plain_act_path_is_unchanged_and_records_no_review() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, _sequence) = observed(&service, &run, "obs-plain").await;

    let outcome = service
        .act(
            "act-plain",
            &run.run_id,
            run.version,
            &run.current_observation
                .as_ref()
                .expect("observation")
                .observation_id,
            invoke(),
        )
        .await
        .expect("the existing path still admits a clean action");
    assert_eq!(outcome.expected_postcondition_met, Some(true));
    assert_eq!(backend.action_calls(), 1);
    assert!(
        adaptive_of(&service, &run.run_id).is_none(),
        "the plain path invented an adaptive review"
    );
}

#[tokio::test]
async fn a_clean_claim_is_admitted_and_projected() {
    for profile in AdaptiveProfile::ALL {
        let (_dir, backend, service, run) = fixture(semantic());
        let (run, sequence) = observed(&service, &run, "obs-clean").await;
        let observation_id = run
            .current_observation
            .as_ref()
            .expect("observation")
            .observation_id
            .clone();

        service
            .act_with_plan(
                "act-clean",
                &run.run_id,
                run.version,
                &observation_id,
                invoke(),
                claim(*profile, &run, sequence),
            )
            .await
            .unwrap_or_else(|error| panic!("{profile:?} refused a clean action: {error}"));

        assert_eq!(backend.action_calls(), 1);
        let summary = adaptive_of(&service, &run.run_id).expect("a review was recorded");
        assert!(summary.admitted);
        assert!(!summary.disagreed);
        assert_eq!(summary.profile, *profile);
        assert_eq!(summary.reason, AdaptiveReason::Admitted);
        assert_eq!(summary.resolved, AdaptiveDisposition::Commit);
        assert_eq!(summary.action_class, ActionClass::Semantic);
        // The bound actually enforced is never looser than the run's own.
        assert!(
            summary.applied_age_bound_millis
                <= ComputerUseLimits::default().max_observation_age_millis
        );
    }
}

#[tokio::test]
async fn the_cheapest_profile_cannot_buy_past_a_kernel_refusal() {
    // The action class is outside the grant, so the policy gate refuses before
    // the seam is ever consulted. The cheap profile and a maximally confident
    // planner change nothing.
    let (_dir, backend, service, run) = fixture(BTreeSet::from([ActionClass::TextEntry]));
    let (run, sequence) = observed(&service, &run, "obs-ungranted").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    let mut confident = claim(AdaptiveProfile::Economy, &run, sequence);
    confident.assessment = AmbiguityAssessment::unambiguous(10_000);

    let error = service
        .act_with_plan(
            "act-ungranted",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            confident,
        )
        .await
        .expect_err("an ungranted class must stay ungranted");
    assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);
    assert_eq!(
        backend.action_calls(),
        0,
        "an ungranted action reached the backend"
    );
    assert!(
        adaptive_of(&service, &run.run_id).is_none(),
        "the seam ran on an action the kernel had already refused"
    );
}

#[tokio::test]
async fn uncertain_work_is_refused_before_the_backend_is_reached() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-uncertain").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    let mut coin_toss = claim(AdaptiveProfile::Balanced, &run, sequence);
    coin_toss.assessment = AmbiguityAssessment {
        candidate_count: 2,
        top_confidence_bps: 9_500,
        runner_up_confidence_bps: 9_400,
    };

    let error = service
        .act_with_plan(
            "act-uncertain",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            coin_toss,
        )
        .await
        .expect_err("a coin toss is not a decision");
    assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
    assert_eq!(backend.action_calls(), 0);

    let summary = adaptive_of(&service, &run.run_id).expect("the refusal was recorded");
    assert!(!summary.admitted);
    assert_eq!(summary.reason, AdaptiveReason::AmbiguityUnresolved);
    assert_eq!(summary.executor, AdaptiveDisposition::Disambiguate);
}

#[tokio::test]
async fn repeating_a_refused_plan_never_quietly_dispatches() {
    // The seam has no retry of its own; this proves the caller cannot get one
    // by simply asking again.
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-repeat").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    for attempt in 0..4 {
        let mut low = claim(AdaptiveProfile::HighAssurance, &run, sequence);
        low.assessment = AmbiguityAssessment::unambiguous(5_000);
        let error = service
            .act_with_plan(
                &format!("act-repeat-{attempt}"),
                &run.run_id,
                run.version,
                &observation_id,
                invoke(),
                low,
            )
            .await
            .expect_err("a below-floor commit is refused every time");
        assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
        assert_eq!(
            backend.action_calls(),
            0,
            "attempt {attempt} dispatched an action the review had refused"
        );
    }
}

#[tokio::test]
async fn a_planner_bound_to_a_superseded_frame_is_refused() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-stale").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    // The kernel is satisfied -- the observation id is current -- but the
    // planner decided against an earlier sequence.
    let stale = AdaptiveClaim {
        observed_sequence: sequence.saturating_sub(1),
        ..claim(AdaptiveProfile::Balanced, &run, sequence)
    };
    let error = service
        .act_with_plan(
            "act-stale",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            stale,
        )
        .await
        .expect_err("a plan bound to an older frame must not act");
    assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    assert_eq!(backend.action_calls(), 0);
    assert_eq!(
        adaptive_of(&service, &run.run_id).expect("recorded").reason,
        AdaptiveReason::StaleFrame
    );
}

#[tokio::test]
async fn a_plan_that_saw_a_different_control_epoch_is_refused() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-epoch").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    let moved = AdaptiveClaim {
        observed_control_epoch: run.control_epoch.saturating_add(1),
        ..claim(AdaptiveProfile::Balanced, &run, sequence)
    };
    let error = service
        .act_with_plan(
            "act-epoch",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            moved,
        )
        .await
        .expect_err("a plan from another control epoch must not act");
    assert_eq!(error.code, ComputerErrorCode::InvalidState);
    assert_eq!(backend.action_calls(), 0);
    assert_eq!(
        adaptive_of(&service, &run.run_id).expect("recorded").reason,
        AdaptiveReason::ControlEpochMoved
    );
}

#[tokio::test]
async fn a_cautious_planner_is_never_overridden() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-cautious").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    let cautious = AdaptiveClaim {
        planner: AdaptiveDisposition::Escalate,
        ..claim(AdaptiveProfile::Economy, &run, sequence)
    };
    let error = service
        .act_with_plan(
            "act-cautious",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            cautious,
        )
        .await
        .expect_err("the planner asked to stop");
    assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
    assert_eq!(backend.action_calls(), 0);

    let summary = adaptive_of(&service, &run.run_id).expect("recorded");
    assert!(summary.disagreed);
    assert_eq!(summary.executor, AdaptiveDisposition::Commit);
    assert_eq!(summary.resolved, AdaptiveDisposition::Escalate);
    assert_eq!(summary.reason, AdaptiveReason::PlannerExecutorDisagreement);
}

#[tokio::test]
async fn forged_approval_json_cannot_admit_a_below_floor_plan() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-approval").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    let below_floor = AmbiguityAssessment::unambiguous(6_500);

    // Unanswered: an outstanding requirement, never consent.
    let mut pending = claim(AdaptiveProfile::Balanced, &run, sequence);
    pending.assessment = below_floor;
    let error = service
        .act_with_plan(
            "act-approval-pending",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            pending.clone(),
        )
        .await
        .expect_err("an unanswered gate is not consent");
    assert_eq!(error.code, ComputerErrorCode::PermissionRequired);
    assert_eq!(backend.action_calls(), 0);

    // A wire claim may contain an approval-shaped object, but the opaque
    // approval field is skipped and cannot become consent.
    let mut forged = serde_json::to_value(&pending).expect("claim serializes");
    assert!(forged.get("approval").is_none());
    forged["approval"] = serde_json::json!({
        "runId": run.run_id,
        "controlEpoch": run.control_epoch,
        "observationId": observation_id,
        "approved": true,
    });
    assert!(serde_json::from_value::<AdaptiveClaim>(forged).is_err());
    assert_eq!(backend.action_calls(), 0);

    // The public integration surface has no way to mint a trusted approval;
    // only host-internal code can do that, so this remains refused.
    assert_eq!(backend.action_calls(), 0);
}

#[tokio::test]
async fn replaying_a_request_id_with_a_different_plan_fails_closed() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-replay").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    service
        .act_with_plan(
            "act-replay",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            claim(AdaptiveProfile::HighAssurance, &run, sequence),
        )
        .await
        .expect("first action");
    assert_eq!(backend.action_calls(), 1);

    // Same request id, a different plan. The receipt covers the claim, so this
    // is a conflict rather than a replay of the first answer.
    let error = service
        .act_with_plan(
            "act-replay",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            claim(AdaptiveProfile::Economy, &run, sequence),
        )
        .await
        .expect_err("a different plan under the same request id must not replay");
    assert_eq!(error.code, ComputerErrorCode::Conflict);
    assert_eq!(backend.action_calls(), 1);
}

#[tokio::test]
async fn the_plain_path_keeps_its_replay_identity() {
    // The seam adds its key to the mutation payload only when a plan is
    // attached, so a plain action still hashes as it always did and a durable
    // receipt written before the seam existed still replays.
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, _sequence) = observed(&service, &run, "obs-replay-plain").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    let first = service
        .act(
            "act-plain-replay",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
        )
        .await
        .expect("first action");
    let replayed = service
        .act(
            "act-plain-replay",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
        )
        .await
        .expect("the same request id replays its recorded answer");
    assert_eq!(first, replayed);
    assert_eq!(
        backend.action_calls(),
        1,
        "a replay dispatched a second time"
    );
}

#[tokio::test]
async fn attaching_a_plan_to_a_used_request_id_fails_closed() {
    let (_dir, backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-mixed").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    service
        .act(
            "act-mixed",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
        )
        .await
        .expect("plain action");
    let error = service
        .act_with_plan(
            "act-mixed",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            claim(AdaptiveProfile::Balanced, &run, sequence),
        )
        .await
        .expect_err("a plan under a spent request id is a conflict, not a replay");
    assert_eq!(error.code, ComputerErrorCode::Conflict);
    assert_eq!(backend.action_calls(), 1);
}

#[tokio::test]
async fn the_projected_review_carries_no_observed_content() {
    let (_dir, _backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-redaction").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    service
        .act_with_plan(
            "act-redaction",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            claim(AdaptiveProfile::HighAssurance, &run, sequence),
        )
        .await
        .expect("clean action");

    let stored = service
        .get_run(&run.run_id)
        .expect("run readable")
        .expect("run exists");
    let projection = project_run_at(&stored, Utc::now());
    let serialized = serde_json::to_string(&projection).expect("projection serializes");
    for forbidden in [ELEMENT_LABEL, ELEMENT_ID, ELEMENT_ROLE, "fixture acted"] {
        assert!(
            !serialized.contains(forbidden),
            "the projection leaked {forbidden:?}"
        );
    }
    assert!(projection.adaptive.is_some());
}

#[tokio::test]
async fn a_profile_can_only_tighten_the_runs_own_staleness_bound() {
    let (_dir, _backend, service, run) = fixture(semantic());
    let (run, sequence) = observed(&service, &run, "obs-bounds").await;
    let observation_id = run
        .current_observation
        .as_ref()
        .expect("observation")
        .observation_id
        .clone();

    service
        .act_with_plan(
            "act-bounds",
            &run.run_id,
            run.version,
            &observation_id,
            invoke(),
            claim(AdaptiveProfile::Economy, &run, sequence),
        )
        .await
        .expect("clean action");

    let summary = adaptive_of(&service, &run.run_id).expect("recorded");
    assert!(
        summary.applied_age_bound_millis <= run.limits.max_observation_age_millis,
        "the cheapest profile widened the run's staleness bound"
    );
}
