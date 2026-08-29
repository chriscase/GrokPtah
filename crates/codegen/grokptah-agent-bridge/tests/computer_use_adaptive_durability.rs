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
    AdaptiveAttemptOutcome, AdaptiveLifecycle, AdaptiveProfile, AdaptiveRecord,
    AdaptiveTurnRequest, CapabilityGeneration, CapabilitySource, ComputerUseService,
    ComputerUseTier, ModelCapabilities, ModelCapabilityEvidence, ObservationFingerprint,
    OperatorCapabilityPolicy, ProfileReason, RuntimeSignal, TaskRisk, TerminalOutcome,
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

/// The model half of the evidence — the only half a caller still supplies,
/// because it is the only half the service cannot read off the live run.
///
/// The host half is now derived inside `begin_adaptive_turn` from the run's own
/// observation and a build constant, so these gates can no longer assert an
/// independent verifier the build does not have. That is the point: High
/// Assurance is unreachable here, and saying so is the honest thing.
fn model_evidence(route: &str, credential: &str, image: bool) -> ModelCapabilityEvidence {
    ModelCapabilityEvidence::from_model_capabilities(
        &capabilities(image),
        true,
        false,
        route,
        credential,
        &OperatorCapabilityPolicy::default(),
    )
}

/// A routine objective. Risk is classified from this and the live frame inside
/// the service; no caller asserts a risk class any more.
const ROUTINE: &str = "save the open document";
/// A destructive objective, in the operator's own words.
const DESTRUCTIVE: &str = "delete the saved document permanently";

fn request<'a>(model: &'a ModelCapabilityEvidence, objective: &'a str) -> AdaptiveTurnRequest<'a> {
    AdaptiveTurnRequest { model, objective }
}

struct Harness {
    _dir: TempDir,
    root: std::path::PathBuf,
    service: Arc<ComputerUseService>,
    owner: Uuid,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = ComputerStore::open(dir.path()).expect("store");
        let root = dir.path().to_path_buf();
        Self {
            _dir: dir,
            root,
            service: Arc::new(ComputerUseService::new(Arc::new(FixtureBackend), store)),
            owner: Uuid::new_v4(),
        }
    }

    /// The durable file behind one run, so a gate can tamper with it the way
    /// anything outside this process would have to. The store sanitizes run ids
    /// into file names, so find the file rather than guessing its name.
    fn run_file(&self, run_id: &str) -> std::path::PathBuf {
        let dir = self.root.join("runs");
        for entry in std::fs::read_dir(&dir).expect("runs directory") {
            let path = entry.expect("entry").path();
            let text = std::fs::read_to_string(&path).expect("read run");
            if text.contains(run_id) {
                return path;
            }
        }
        panic!("no durable file for run {run_id} under {}", dir.display());
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
    let evidence = model_evidence("route-1", "cred-1", true);

    service
        .begin_adaptive_turn(&run.run_id, owner, request(&evidence, ROUTINE))
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
    let evidence = model_evidence("route-1", "cred-1", true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
        .expect("first turn admitted");
    harness
        .service
        .record_adaptive_attempt(&run.run_id)
        .expect("count the attempt");
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
    let downgraded = ModelCapabilityEvidence::from_model_capabilities(
        &ModelCapabilities {
            computer_use_tier: ComputerUseTier::Observe,
            ..capabilities(true)
        },
        true,
        false,
        "route-1",
        "cred-1",
        &OperatorCapabilityPolicy::default(),
    );
    let error = harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&downgraded, ROUTINE))
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
    let evidence = model_evidence("route-1", "cred-1", true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
        .expect("routine turn admitted");
    harness
        .service
        .record_adaptive_attempt(&run.run_id)
        .expect("count the attempt");
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
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, DESTRUCTIVE))
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
    let evidence = model_evidence("route-1", "cred-1", false);

    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
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
    let evidence = model_evidence("route-1", "cred-1", true);

    let first = harness.ready_run().await;
    harness
        .service
        .begin_adaptive_turn(&first.run_id, harness.owner, request(&evidence, ROUTINE))
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
        .begin_adaptive_turn(&second.run_id, harness.owner, request(&evidence, ROUTINE))
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
    let evidence = model_evidence("route-1", "cred-1", true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
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
    // Stronger than a field-name check: no 64-character hex run appears
    // anywhere in the payload, so the digest cannot have leaked under some
    // other key either.
    assert!(
        !wire
            .as_bytes()
            .windows(64)
            .any(|window| window.iter().all(u8::is_ascii_hexdigit)),
        "projection contains a digest-shaped value: {wire}"
    );
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
    let evidence = model_evidence("route-1", "cred-1", true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
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

/// **The wire shape is pinned, in Rust.**
///
/// `ComputerRunProjection` is the read shape the cockpit, the MCP surface, and
/// SDK consumers all share, so adding a key to it changes a public contract.
/// A Node conformance fixture already pins this set over real loopback HTTP,
/// but that fixture needs an `npm ci`, so it only runs in CI — and adding
/// `adaptive` to the projection sailed past every local check and broke it
/// there. This gate is the same pin in the ordinary bridge suite: a new field
/// on either shape fails here first, in a second, with the two lists to
/// reconcile printed side by side.
///
/// If this fails, the fix is to decide whether the new key belongs on the wire
/// at all, and if it does, to update **both** this list and `ADAPTIVE_KEYS` /
/// `PROJECTION_KEYS` in `tests/mcp_sdk_interop/run_computer_reads_smoke.mjs`.
#[tokio::test]
async fn the_projection_wire_shape_is_pinned() {
    /// Mirrors `PROJECTION_KEYS` in `run_computer_reads_smoke.mjs`.
    const PROJECTION_KEYS: &[&str] = &[
        "adaptive",
        "agentActive",
        "campaignId",
        "controlDisposition",
        "controlEpoch",
        "createdAt",
        "endedAt",
        "eventRange",
        "grant",
        "lastError",
        "lastOutcome",
        "observation",
        "ownerSessionId",
        "parentRunId",
        "progress",
        "runId",
        "startedAt",
        "state",
        "target",
        "terminal",
        "updatedAt",
        "version",
    ];
    /// Mirrors `ADAPTIVE_KEYS` in `run_computer_reads_smoke.mjs`.
    const ADAPTIVE_KEYS: &[&str] = &[
        "budget",
        "capability",
        "cost",
        "escalations",
        "lifecycle",
        "message",
        "observationTruncated",
        "profile",
        "profileDisplayName",
        "reason",
        "requiresIndependentVerifier",
        "revision",
        "risk",
        "riskHighWater",
        "safetyFloor",
        "stationaryRepeats",
        "terminal",
    ];

    /// Mirrors `ADAPTIVE_BUDGET_KEYS` in `run_computer_reads_smoke.mjs`.
    const BUDGET_KEYS: &[&str] = &[
        "keyChordAllowed",
        "maxModelCalls",
        "maxObservationBytes",
        "maxObservationElements",
        "maxTurnMillis",
        "observationDetail",
        "pointerFallbackAllowed",
    ];
    /// Mirrors `ADAPTIVE_CAPABILITY_KEYS` in `run_computer_reads_smoke.mjs`.
    const CAPABILITY_KEYS: &[&str] = &[
        "attribution",
        "ceiling",
        "declaredCapabilityTrusted",
        "durableAuthority",
        "generation",
        "hostIndependentVerifier",
        "hostScreenshotCapture",
        "imageInput",
        "qualifiedVisualPath",
        "sessionMeasured",
        "structuredTools",
        "syntheticOnly",
        "tier",
    ];
    /// Mirrors `ADAPTIVE_COST_KEYS` in `run_computer_reads_smoke.mjs`.
    const COST_KEYS: &[&str] = &[
        "acceptedAttempts",
        "completionTokens",
        "failedAttempts",
        "observationBytes",
        "promptTokens",
        "providerAttempts",
        "screenshotBytes",
    ];

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    let harness = Harness::new();
    let run = harness.ready_run().await;
    let evidence = model_evidence("route-1", "cred-1", true);
    harness
        .service
        .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
        .expect("turn");

    let projection = harness
        .service
        .project_session_run(harness.owner, &run.run_id, Utc::now())
        .expect("project");
    let wire = serde_json::to_value(&projection).expect("serialize");

    assert_eq!(
        keys(&wire),
        PROJECTION_KEYS,
        "the run projection's wire keys changed"
    );
    assert_eq!(
        keys(&wire["adaptive"]),
        ADAPTIVE_KEYS,
        "the adaptive projection's wire keys changed"
    );
    assert_eq!(
        keys(&wire["adaptive"]["budget"]),
        BUDGET_KEYS,
        "the adaptive budget's wire keys changed"
    );
    assert_eq!(
        keys(&wire["adaptive"]["capability"]),
        CAPABILITY_KEYS,
        "the adaptive capability evidence's wire keys changed"
    );
    assert_eq!(
        keys(&wire["adaptive"]["cost"]),
        COST_KEYS,
        "the adaptive cost ledger's wire keys changed"
    );
}

/// **A record that fails its own invariants is not authority.**
///
/// The durable record is a file. It can be truncated by a full disk, restored
/// from a backup taken mid-write, or edited on purpose — and every field in it
/// is an input to an authority decision. Each case below tampers with exactly
/// one field, in the direction that would *help* an attacker, and asserts the
/// record comes back terminal rather than permissive.
#[tokio::test]
async fn a_tampered_record_is_refused_rather_than_trusted() {
    /// One tamper case: a label, and the single edit it makes.
    type TamperCase = (&'static str, fn(&mut AdaptiveRecord));

    // Each case names what it edits and why that edit is worth making.
    let cases: Vec<TamperCase> = vec![
        ("claim a profile the evidence cannot support", |record| {
            record.profile = AdaptiveProfile::HighAssurance;
        }),
        ("inflate accepted attempts to hide spend", |record| {
            record.cost.accepted_attempts = record.cost.accepted_attempts.saturating_add(40);
        }),
        ("claim screenshot bytes were sent to a model", |record| {
            record.cost.screenshot_bytes = 1;
        }),
        ("lower the risk high-water mark", |record| {
            record.risk_high_water = TaskRisk::Routine;
            record.decision.risk = TaskRisk::Destructive;
        }),
        (
            "resurrect a terminal record by clearing its outcome",
            |record| {
                record.lifecycle = AdaptiveLifecycle::Stopped;
                record.terminal = None;
            },
        ),
        ("forge an admitted turn on a terminal record", |record| {
            record.lifecycle = AdaptiveLifecycle::Stopped;
            record.terminal = Some(TerminalOutcome {
                lifecycle: AdaptiveLifecycle::Stopped,
                reason: ProfileReason::BudgetExhausted,
                profile: record.profile,
                required_profile: None,
            });
            record.active_permit = Some("permit-forged".into());
        }),
    ];

    for (label, tamper) in cases {
        let harness = Harness::new();
        let run = harness.ready_run().await;
        let evidence = model_evidence("route-1", "cred-1", true);
        harness
            .service
            .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
            .expect("turn admitted");
        harness
            .service
            .record_adaptive_attempt(&run.run_id)
            .expect("count the attempt");
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

        // Edit the file behind the store's back, exactly as anything outside
        // this process would have to.
        let path = harness.run_file(&run.run_id);
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        let mut record: grokptah_agent_bridge::AdaptiveRecord =
            serde_json::from_value(raw["adaptive"].clone()).expect("record");
        assert!(
            record.check_invariants().is_ok(),
            "{label}: the untampered record must be valid to start with"
        );
        tamper(&mut record);
        raw["adaptive"] = serde_json::to_value(&record).expect("serialize");
        std::fs::write(&path, serde_json::to_string(&raw).expect("encode")).expect("write");

        // Reading it is enough: the record comes back terminal, not permissive.
        let loaded = harness
            .service
            .get_run(&run.run_id)
            .expect("load")
            .expect("run")
            .adaptive
            .expect("record survives so the operator can see what happened");
        assert_eq!(
            loaded.lifecycle,
            AdaptiveLifecycle::Stopped,
            "{label}: a record that fails its invariants must not stay live"
        );
        assert_eq!(
            loaded.terminal.as_ref().map(|terminal| terminal.reason),
            Some(ProfileReason::RecordInvalid),
            "{label}: the stop must name why"
        );
        assert_eq!(
            loaded.active_permit, None,
            "{label}: an invalid record holds no admitted turn"
        );

        // And nothing can be admitted against it afterwards.
        let refused = harness
            .service
            .begin_adaptive_turn(&run.run_id, harness.owner, request(&evidence, ROUTINE))
            .expect_err(&format!("{label}: an invalid record must admit nothing"));
        assert_eq!(refused.code, ComputerErrorCode::Unauthorized, "{label}");

        // The operator can read what happened, in words.
        let projection = harness
            .service
            .adaptive_projection(&run.run_id)
            .expect("projection")
            .expect("record");
        assert_eq!(
            projection.terminal.as_ref().map(|t| t.reason),
            Some(ProfileReason::RecordInvalid),
            "{label}"
        );
        assert!(!projection
            .terminal
            .as_ref()
            .expect("terminal")
            .message
            .is_empty());
    }
}
