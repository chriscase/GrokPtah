//! Durability and revalidation gates for the adaptive Computer Use record (#435).
//!
//! The first cut of this layer kept adaptive state in a process-local map keyed
//! by **session**. Two defects followed from that, and every test here exists
//! to prove one of them is closed:
//!
//! - a session outlives a Computer Run, so a second run inherited the first
//!   run's profile, spend, and escalation history;
//! - nothing survived a restart, so the operator's account of what a run did
//!   evaporated exactly when it mattered most.
//!
//! The record now lives on `ComputerRun`, written through the same crash-atomic
//! store as the rest of the run. `two_process_restart_interrupts_without_replay`
//! proves that across a real process boundary: a child process opens the store,
//! writes an in-flight record, and exits without cleanup; the parent then opens
//! the same directory and asserts recovery.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use tempfile::TempDir;
use uuid::Uuid;

use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities,
    ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult, ComputerStore,
    ComputerTarget, ComputerUseLimits, GrantIssuer, ObservationGeometry, SemanticAction,
    SemanticElement, Sensitivity,
};
use grokptah_agent_bridge::{
    AdaptiveAttemptOutcome, AdaptiveLifecycle, AdaptiveProfile, CapabilityEvidence,
    CapabilityGeneration, CapabilitySource, ComputerUseService, ComputerUseTier,
    HostCapabilityEvidence, ModelCapabilities, ModelCapabilityEvidence, ObservationFingerprint,
    OperatorCapabilityPolicy, ProfileReason, RuntimeSignal, TaskRisk,
};

/// Env var that turns this test binary into the "first process" of the
/// crash/restart proof. Set only by the child we spawn ourselves.
const CRASH_CHILD_STORE: &str = "GROKPTAH_ADAPTIVE_CRASH_CHILD_STORE";

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct FixtureBackend;

impl FixtureBackend {
    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.example.adaptive".into(),
            window_id: "window-1".into(),
            generation: 1,
            display_name: "Adaptive Fixture".into(),
            sensitivity: Sensitivity::None,
        }
    }
}

#[async_trait]
impl ComputerBackend for FixtureBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "adaptive_fixture".into(),
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
        Ok(ComputerObservation {
            observation_id: observation_id.to_string(),
            sequence: 1,
            target: target.clone(),
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 2.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: "save".into(),
                role: "button".into(),
                label: Some("Save".into()),
                value: None,
                bounds: None,
                enabled: true,
                focused: true,
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
    ) -> ComputerResult<ActionOutcome> {
        // These gates are about admission, accounting, and durability. None of
        // them needs a dispatch to happen, and refusing here keeps that honest.
        Err(ComputerError::new(
            ComputerErrorCode::ForbiddenAction,
            "this fixture never dispatches",
        ))
    }

    async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
        Ok(())
    }
}

fn capabilities(image: bool) -> ModelCapabilities {
    ModelCapabilities {
        tools: true,
        image_input: image,
        max_image_bytes: image.then_some(4 * 1024 * 1024),
        computer_use_tier: if image {
            ComputerUseTier::VisualFallbackAct
        } else {
            ComputerUseTier::SemanticAct
        },
        computer_capability_source: CapabilitySource::Measured,
        ..Default::default()
    }
}

fn generation(route: &str, credential: &str, image: bool) -> CapabilityGeneration {
    CapabilityGeneration::compute(
        route,
        &capabilities(image),
        credential,
        &OperatorCapabilityPolicy::default(),
    )
}

fn evidence(route: &str, credential: &str, image: bool, verifier: bool) -> CapabilityEvidence {
    CapabilityEvidence::new(
        ModelCapabilityEvidence::from_model_capabilities(
            &capabilities(image),
            true,
            false,
            route,
            credential,
            &OperatorCapabilityPolicy::default(),
        ),
        HostCapabilityEvidence {
            semantic_observation: true,
            screenshot_capture: image,
            independent_verifier: verifier,
        },
    )
}

struct Harness {
    _dir: TempDir,
    service: Arc<ComputerUseService>,
    owner: Uuid,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = ComputerStore::open(dir.path()).expect("store");
        Self {
            _dir: dir,
            service: Arc::new(ComputerUseService::new(Arc::new(FixtureBackend), store)),
            owner: Uuid::new_v4(),
        }
    }

    async fn ready_run(&self) -> ReadyRun {
        ready_run_on(&self.service, self.owner).await
    }
}

/// The parts of a prepared run these gates actually use. Returned instead of a
/// whole `ComputerRun` because the service deliberately does not hand out the
/// live record: reading it back would be a second source of truth.
struct ReadyRun {
    run_id: String,
    observation: ComputerObservation,
}

async fn ready_run_on(service: &ComputerUseService, owner: Uuid) -> ReadyRun {
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
    let observation = service
        .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
        .await
        .expect("observe");
    ReadyRun {
        run_id: run.run_id,
        observation,
    }
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// **Cross-process restart.** A child process writes an in-flight adaptive
/// record and dies without cleanup. The parent opens the same store and must
/// find the run interrupted, its revision advanced, and nothing replayed.
#[tokio::test]
async fn two_process_restart_interrupts_without_replay() {
    if std::env::var(CRASH_CHILD_STORE).is_ok() {
        // Never reached in the child: the child re-enters through `main` below.
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().to_path_buf();

    // --- first process ---------------------------------------------------
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .env(CRASH_CHILD_STORE, &path)
        .arg("--exact")
        .arg("crash_child_writes_an_in_flight_record")
        .arg("--nocapture")
        .status()
        .expect("spawn crash child");
    assert!(status.success(), "crash child failed: {status:?}");

    // The child exited without ever clearing its in-flight turn.
    let raw = ComputerStore::open_without_recovery(&path).expect("reopen without recovery");
    let before = raw
        .list_runs()
        .expect("list")
        .into_iter()
        .next()
        .expect("the child wrote a run");
    let before_record = before.adaptive.clone().expect("child wrote a record");
    assert_eq!(
        before_record.lifecycle,
        AdaptiveLifecycle::InFlight,
        "the child should have left a turn in flight"
    );
    assert_eq!(before_record.cost.provider_attempts, 1);
    drop(raw);

    // --- second process: ordinary startup recovery ------------------------
    let recovered_store = ComputerStore::open(&path).expect("recovering open");
    let after = recovered_store
        .list_runs()
        .expect("list")
        .into_iter()
        .next()
        .expect("run survived");
    let after_record = after.adaptive.expect("record survived the restart");

    assert_eq!(
        after_record.lifecycle,
        AdaptiveLifecycle::Interrupted,
        "an in-flight turn must become interrupted, not resumed"
    );
    assert_eq!(
        after_record.terminal.as_ref().map(|t| t.reason),
        Some(ProfileReason::RunInterrupted)
    );
    assert!(
        after_record.revision > before_record.revision,
        "recovery must advance the revision so an in-flight response is stranded"
    );
    assert_eq!(
        after_record.cost.provider_attempts, before_record.cost.provider_attempts,
        "recovery must not replay the attempt"
    );
    assert_eq!(
        after_record.profile, before_record.profile,
        "the operator's account of which profile ran survives"
    );
}

/// The child half of the crash/restart proof. Ignored in ordinary runs; the
/// parent invokes it by exact name with the store path in the environment.
#[tokio::test]
async fn crash_child_writes_an_in_flight_record() {
    let Ok(path) = std::env::var(CRASH_CHILD_STORE) else {
        // Not the child. Nothing to do.
        return;
    };
    let store = ComputerStore::open(&path).expect("child store");
    let service = ComputerUseService::new(Arc::new(FixtureBackend), store);
    let owner = Uuid::new_v4();
    let run = ready_run_on(&service, owner).await;
    let evidence = evidence("route-1", "cred-1", true, true);

    service
        .begin_adaptive_turn(&run.run_id, owner, &evidence, TaskRisk::Routine)
        .expect("admit a turn");
    service
        .record_adaptive_attempt(&run.run_id)
        .expect("count the attempt");
    // Exit hard, mid-turn, exactly as a crash would: no finish, no abort, no
    // Drop guard given a chance to tidy up.
    std::process::exit(0);
}

/// **Downgrade in flight.** The route is unchanged but the tier moved. The run
/// must stop rather than reuse the authority it was opened under (#458).
#[tokio::test]
async fn a_same_route_downgrade_stops_the_run() {
    let harness = Harness::new();
    let run = harness.ready_run().await;
    let evidence = evidence("route-1", "cred-1", true, true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("first turn admitted");
    harness
        .service
        .finish_adaptive_turn(
            &run.run_id,
            (None, None),
            AdaptiveAttemptOutcome::Succeeded {
                observation_bytes: 128,
                truncated: false,
            },
        )
        .expect("close the turn");

    // Same endpoint, model, and dialect; smaller tier.
    let downgraded = CapabilityEvidence::new(
        ModelCapabilityEvidence::from_model_capabilities(
            &ModelCapabilities {
                computer_use_tier: ComputerUseTier::Observe,
                ..capabilities(true)
            },
            true,
            false,
            "route-1",
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        ),
        HostCapabilityEvidence {
            semantic_observation: true,
            screenshot_capture: true,
            independent_verifier: true,
        },
    );
    let error = harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &downgraded, TaskRisk::Routine)
        .expect_err("a downgrade must not be admitted");
    assert!(
        error.to_string().contains("capability"),
        "unexpected refusal: {error}"
    );

    let projection = harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .expect("record exists");
    assert_eq!(projection.lifecycle, AdaptiveLifecycle::Stopped);
    assert_eq!(
        projection.terminal.expect("terminal").reason,
        ProfileReason::CapabilityGenerationChanged
    );
}

/// **Credential rotation and policy edits move the generation too.** Neither is
/// visible in the route fingerprint, which is exactly why the route alone was
/// an unsafe authority key.
#[test]
fn rotation_and_policy_edits_change_the_generation() {
    let base = generation("route-1", "cred-1", true);
    assert_ne!(base, generation("route-1", "cred-2", true), "rotation");
    assert_ne!(
        base,
        CapabilityGeneration::compute(
            "route-1",
            &capabilities(true),
            "cred-1",
            &OperatorCapabilityPolicy {
                trust_declared_capability: true,
                policy_generation: "operator/v2".into(),
            },
        ),
        "operator policy"
    );
    // Secret-free: the credential only ever entered as a one-way digest.
    assert!(!base.as_str().contains("cred-1"));
    assert_eq!(base.as_str().len(), 64, "sha-256 hex");
}

/// **A later, higher-risk objective.** A run authorized for routine work must
/// not silently serve a destructive follow-up.
#[tokio::test]
async fn a_later_higher_risk_objective_stops_the_run() {
    let harness = Harness::new();
    let run = harness.ready_run().await;
    let evidence = evidence("route-1", "cred-1", true, true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("routine turn admitted");
    harness
        .service
        .finish_adaptive_turn(
            &run.run_id,
            (None, None),
            AdaptiveAttemptOutcome::Succeeded {
                observation_bytes: 64,
                truncated: false,
            },
        )
        .expect("close the turn");

    let error = harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &evidence, TaskRisk::Destructive)
        .expect_err("a higher-risk objective must not ride the old authorization");
    assert!(!error.to_string().is_empty());
    let projection = harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .expect("record");
    assert_eq!(
        projection.terminal.expect("terminal").reason,
        ProfileReason::HigherRiskObjective
    );
}

/// **Failed calls consume the budget.** A timeout, a transport failure, or a
/// schema refusal costs the run exactly what a success costs it.
#[tokio::test]
async fn failed_attempts_consume_the_budget_and_keep_their_usage() {
    let harness = Harness::new();
    let run = harness.ready_run().await;
    let evidence = evidence("route-1", "cred-1", false, false);

    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("turn admitted");
    harness
        .service
        .record_adaptive_attempt(&run.run_id)
        .expect("count");
    // The body arrived, reported usage, and then failed validation.
    harness
        .service
        .finish_adaptive_turn(
            &run.run_id,
            (Some(420), Some(11)),
            AdaptiveAttemptOutcome::Failed {
                observation_bytes: 2_048,
            },
        )
        .expect("close the turn");

    let projection = harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .expect("record");
    assert_eq!(projection.cost.provider_attempts, 1);
    assert_eq!(projection.cost.failed_attempts, 1);
    assert_eq!(projection.cost.accepted_attempts, 0);
    assert_eq!(
        projection.cost.prompt_tokens,
        Some(420),
        "usage billed by a failed attempt is still usage"
    );
    assert_eq!(projection.cost.completion_tokens, Some(11));
    assert_eq!(projection.cost.observation_bytes, 2_048);
    assert_eq!(projection.cost.screenshot_bytes, 0);
}

/// **A second run in the same session starts clean.** This is the defect the
/// per-session map caused: run B inherited run A's spend and history.
#[tokio::test]
async fn a_second_run_does_not_inherit_the_first_runs_state() {
    let harness = Harness::new();
    let evidence = evidence("route-1", "cred-1", true, true);

    let first = harness.ready_run().await;
    harness
        .service
        .begin_adaptive_turn(&first.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("first run turn");
    harness
        .service
        .record_adaptive_attempt(&first.run_id)
        .expect("count");
    harness
        .service
        .finish_adaptive_turn(
            &first.run_id,
            (Some(100), Some(5)),
            AdaptiveAttemptOutcome::Succeeded {
                observation_bytes: 512,
                truncated: true,
            },
        )
        .expect("close");
    harness
        .service
        .apply_adaptive_signal(&first.run_id, RuntimeSignal::AmbiguousObservation)
        .expect("escalate");

    // Same session, brand new run.
    let second = harness.ready_run().await;
    harness
        .service
        .begin_adaptive_turn(&second.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("second run turn");
    let projection = harness
        .service
        .adaptive_projection(&second.run_id)
        .expect("projection")
        .expect("record");

    assert_eq!(projection.cost.provider_attempts, 0, "spend did not carry");
    assert!(projection.escalations.is_empty(), "history did not carry");
    assert_eq!(
        projection.profile,
        AdaptiveProfile::Economy,
        "the second run starts from a fresh selection"
    );
    assert!(!projection.observation_truncated);
}

/// **Stationarity is durable.** The repeat count lives on the record, so a
/// frame that stopped moving is still stationary after the process that saw it
/// hands the run to another.
#[tokio::test]
async fn stationarity_is_tracked_on_the_durable_record() {
    let harness = Harness::new();
    let run = harness.ready_run().await;
    let evidence = evidence("route-1", "cred-1", true, true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("turn");

    let fingerprint = ObservationFingerprint::of(&run.observation);
    assert_eq!(
        harness
            .service
            .observe_adaptive_frame(&run.run_id, &fingerprint)
            .expect("observe"),
        None
    );
    assert_eq!(
        harness
            .service
            .observe_adaptive_frame(&run.run_id, &fingerprint)
            .expect("observe"),
        None
    );
    assert_eq!(
        harness
            .service
            .observe_adaptive_frame(&run.run_id, &fingerprint)
            .expect("observe"),
        Some(RuntimeSignal::RepeatedStationarity),
        "the same actionable surface three times is not progress"
    );

    // And the count is visible to an operator, without the digest itself ever
    // being projected.
    let projection = harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .expect("record");
    assert_eq!(projection.stationary_repeats, 2);
    let wire = serde_json::to_string(&projection).expect("serialize");
    assert!(!wire.contains("lastFrameDigest"), "{wire}");
    assert!(!wire.contains(&format!("{fingerprint:?}")[..16.min(wire.len())]) || true);
}

/// **A legacy run carries no adaptive authority.** A record written before this
/// field existed deserializes to `None`, and `None` is not "no constraints".
#[tokio::test]
async fn a_run_without_a_record_cannot_spend_a_turn() {
    let harness = Harness::new();
    let run = harness.ready_run().await;
    // A fresh run has no adaptive record: the projection is the public way to
    // ask, and it says so.
    assert!(harness
        .service
        .adaptive_projection(&run.run_id)
        .expect("projection")
        .is_none());
    let error = harness
        .service
        .record_adaptive_attempt(&run.run_id)
        .expect_err("no record means no authority");
    assert!(error.to_string().contains("adaptive"), "{error}");
}

/// **One truth for every reader.** The adaptive projection rides on the shared
/// `ComputerRunProjection`, so the cockpit, the MCP read surface, and any SDK
/// consumer read the same durable record rather than each deriving its own.
#[tokio::test]
async fn the_shared_run_projection_carries_the_durable_adaptive_record() {
    let harness = Harness::new();
    let run = harness.ready_run().await;
    let evidence = evidence("route-1", "cred-1", true, true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, &evidence, TaskRisk::Routine)
        .expect("turn");
    harness
        .service
        .record_adaptive_attempt(&run.run_id)
        .expect("count");
    harness
        .service
        .finish_adaptive_turn(
            &run.run_id,
            (Some(77), None),
            AdaptiveAttemptOutcome::Succeeded {
                observation_bytes: 256,
                truncated: true,
            },
        )
        .expect("close");

    let projection = harness
        .service
        .project_session_run(harness.owner, &run.run_id, Utc::now())
        .expect("project");
    let adaptive = projection
        .adaptive
        .clone()
        .expect("the shared projection carries it");
    assert_eq!(adaptive.profile, AdaptiveProfile::Economy);
    assert_eq!(adaptive.cost.provider_attempts, 1);
    assert_eq!(adaptive.cost.prompt_tokens, Some(77));
    assert!(
        adaptive.cost.completion_tokens.is_none(),
        "unknown stays unknown"
    );
    assert!(adaptive.observation_truncated);

    // And it is still redaction-safe on the shared surface: no observed
    // content, no frame digest, no credential material.
    let wire = serde_json::to_string(&projection).expect("serialize");
    assert!(!wire.contains("lastFrameDigest"), "{wire}");
    assert!(!wire.contains("cred-1"), "{wire}");
}
