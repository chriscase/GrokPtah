//! `computer.control` human-approval receipt boundary.
//!
//! `computer.control` is advertised with `human_gate: true`. These tests pin
//! the property that makes that advertisement true: **only** a host-issued,
//! fully bound, one-time receipt authorizes Computer Use control. An
//! authenticated bearer token, an initialized MCP session, and a
//! caller-supplied Boolean are each insufficient — individually and together.
//!
//! Every clock read is an explicit parameter, so expiry and lifecycle
//! assertions are deterministic rather than wall-clock dependent. No live
//! desktop, credential, provider, or OS input is involved: the backend is the
//! in-tree simulator, wrapped so the tests can prove that no observation or
//! action ever reaches it before a receipt has been spent.

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    advertised_capabilities, canonical_workspace_string, capability_revision,
    capability_revision_of, home_override_serial, set_grokptah_home_override, start_control_server,
    ActionClass, ActionOutcome, AgentHost, ApprovalPresentation, ApprovalPrincipal,
    ApprovalProjection, ApprovalStatus, CapabilityAvailability, ComputerAction, ComputerBackend,
    ComputerCapabilities, ComputerClientIdentity, ComputerError, ComputerErrorCode,
    ComputerGrantRequest, ComputerObservation, ComputerRun, ComputerRunController, ComputerStore,
    ComputerTarget, ComputerUseLimits, ComputerUseService, HostConfig, IssuedApproval,
    SimulatorBackend,
};
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};
use uuid::Uuid;

// ── Instrumented backend ─────────────────────────────────────────────────
//
// Records every operation that would touch the screen or the input stream.
// `status_probe` is read at the moment of each call, so the log answers not
// just "did input happen" but "what was the receipt's state when it did".

type StatusProbe = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackendCall {
    operation: &'static str,
    approval_status_at_call: String,
}

struct RecordingBackend {
    inner: SimulatorBackend,
    calls: Arc<Mutex<Vec<BackendCall>>>,
    probe: Arc<Mutex<Option<StatusProbe>>>,
}

impl std::fmt::Debug for RecordingBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingBackend").finish_non_exhaustive()
    }
}

impl RecordingBackend {
    fn new() -> (
        Arc<Self>,
        Arc<Mutex<Vec<BackendCall>>>,
        Arc<Mutex<Option<StatusProbe>>>,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = Arc::new(Mutex::new(None));
        let backend = Arc::new(Self {
            inner: SimulatorBackend::new(),
            calls: calls.clone(),
            probe: probe.clone(),
        });
        (backend, calls, probe)
    }

    fn record(&self, operation: &'static str) {
        let status = self
            .probe
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(|| "no-probe".to_string(), |probe| probe());
        self.calls.lock().unwrap().push(BackendCall {
            operation,
            approval_status_at_call: status,
        });
    }
}

#[async_trait]
impl ComputerBackend for RecordingBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        self.inner.capabilities()
    }

    async fn observe(
        &self,
        run_id: &str,
        target: &ComputerTarget,
        limits: &ComputerUseLimits,
    ) -> Result<ComputerObservation, ComputerError> {
        self.record("observe");
        self.inner.observe(run_id, target, limits).await
    }

    async fn act(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> Result<ActionOutcome, ComputerError> {
        self.record("act");
        self.inner.act(run_id, observation, action).await
    }

    async fn read_evidence(
        &self,
        run_id: &str,
        asset_id: &str,
    ) -> Result<Option<Vec<u8>>, ComputerError> {
        self.record("read_evidence");
        self.inner.read_evidence(run_id, asset_id).await
    }

    async fn cancel(&self, run_id: &str) -> Result<(), ComputerError> {
        self.inner.cancel(run_id).await
    }
}

// ── Fixture ──────────────────────────────────────────────────────────────

struct Fixture {
    _dir: TempDir,
    store: ComputerStore,
    service: Arc<ComputerUseService>,
    reader: Arc<ComputerUseService>,
    calls: Arc<Mutex<Vec<BackendCall>>>,
    probe: Arc<Mutex<Option<StatusProbe>>>,
    owner: Uuid,
    workspace: String,
    run: ComputerRun,
}

const WORKSPACE: &str = "/approved/workspace";
const OTHER_WORKSPACE: &str = "/approved/other";

fn principal() -> ApprovalPrincipal {
    ApprovalPrincipal {
        principal_id: "primary".into(),
        token_fingerprint: "a".repeat(64),
        mcp_session_id: "mcp-session-alpha".into(),
        client_actor_id: "coordinator@1.0#mcp-session-alpha".into(),
    }
}

/// Deterministic *relative* clock.
///
/// The base is read once from the real clock so durable retention windows and
/// grant lifetimes behave as they do in production; every assertion depends
/// only on the offsets, which are fixed.
fn at(minute: i64) -> chrono::DateTime<Utc> {
    static BASE: std::sync::OnceLock<chrono::DateTime<Utc>> = std::sync::OnceLock::new();
    *BASE.get_or_init(Utc::now) + Duration::minutes(minute)
}

fn grant(classes: &[ActionClass], uses: Option<u32>, ttl_ms: u64) -> ComputerGrantRequest {
    ComputerGrantRequest {
        action_classes: classes.iter().copied().collect::<BTreeSet<_>>(),
        ttl_ms,
        uses_remaining: uses,
    }
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path()).unwrap();
        let (backend, calls, probe) = RecordingBackend::new();
        let service = Arc::new(ComputerUseService::new(backend, store.clone()));
        // A second handle over the same durable ledger with a plain backend,
        // so the recording backend can read approval state without an Arc
        // cycle through the service that owns it.
        let reader = Arc::new(ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            store.clone(),
        ));
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "fixture-create",
                owner,
                Some(WORKSPACE.to_string()),
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        Self {
            _dir: dir,
            store,
            service,
            reader,
            calls,
            probe,
            owner,
            workspace: WORKSPACE.to_string(),
            run,
        }
    }

    /// Request an approval and have the trusted host issue the receipt.
    fn issued_receipt(
        &self,
        request_id: &str,
        approved: &ComputerGrantRequest,
        revision: &str,
        now: chrono::DateTime<Utc>,
    ) -> ApprovalPresentation {
        let issued = self
            .service
            .request_control_approval(
                request_id,
                self.owner,
                &self.workspace,
                &self.run.run_id,
                self.run.version,
                principal(),
                revision,
                approved,
                now,
            )
            .unwrap();
        self.service
            .decide_control_approval(&issued.record.approval_id, self.owner, true, revision, now)
            .unwrap();
        ApprovalPresentation {
            approval_id: issued.record.approval_id,
            nonce: issued.nonce,
        }
    }

    fn arm_probe(&self, approval_id: &str) {
        let reader = self.reader.clone();
        let approval_id = approval_id.to_string();
        let owner = self.owner;
        let workspace = self.workspace.clone();
        *self.probe.lock().unwrap() = Some(Arc::new(move || {
            reader
                .read_control_approval(&approval_id, &principal(), owner, &workspace, Utc::now())
                .map(|projection| format!("{:?}", projection.status))
                .unwrap_or_else(|error| format!("unreadable: {error}"))
        }));
    }

    fn backend_calls(&self) -> Vec<BackendCall> {
        self.calls.lock().unwrap().clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize(
        &self,
        request_id: &str,
        requested: &ComputerGrantRequest,
        presentation: &ApprovalPresentation,
        revision: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<ComputerRun, ComputerError> {
        self.service.authorize_with_receipt(
            request_id,
            self.owner,
            &self.workspace,
            &self.run.run_id,
            self.run.version,
            principal(),
            revision,
            requested.clone(),
            presentation,
            now,
        )
    }

    fn granted(&self) -> bool {
        self.service
            .get_run(&self.run.run_id)
            .unwrap()
            .and_then(|run| run.grant)
            .is_some()
    }
}

// ── Service-level receipt semantics ──────────────────────────────────────

#[test]
fn a_valid_host_receipt_is_required_and_grants_bounded_control() {
    let fixture = Fixture::new();
    let approved = grant(
        &[ActionClass::Semantic, ActionClass::TextEntry],
        Some(4),
        60_000,
    );
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));
    fixture.arm_probe(&receipt.approval_id);

    assert!(
        !fixture.granted(),
        "an issued receipt must not grant control until it is spent"
    );
    assert!(
        fixture.backend_calls().is_empty(),
        "requesting and issuing an approval must touch no screen or input"
    );

    let run = fixture
        .authorize(
            "authorize-1",
            &grant(&[ActionClass::Semantic], Some(2), 30_000),
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect("a valid host receipt authorizes control");

    let issued_grant = run.grant.expect("control authority is attached");
    assert_eq!(
        issued_grant.action_classes,
        BTreeSet::from([ActionClass::Semantic]),
        "the grant carries the narrowed request, not the broader approval"
    );
    assert_eq!(issued_grant.uses_remaining, Some(2));
    assert_eq!(
        issued_grant.expires_at,
        at(1) + Duration::milliseconds(30_000)
    );

    let projection = fixture
        .reader
        .read_control_approval(
            &receipt.approval_id,
            &principal(),
            fixture.owner,
            &fixture.workspace,
            at(1),
        )
        .unwrap();
    assert_eq!(projection.status, ApprovalStatus::Consumed);
    assert_eq!(projection.consumed_at, Some(at(1)));
    assert!(
        fixture.backend_calls().is_empty(),
        "authorization itself performs no physical input or frame operation"
    );
}

#[tokio::test]
async fn no_frame_or_input_operation_precedes_receipt_consumption() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(2), 60_000);

    // Before any approval exists, an observation is refused and the backend
    // is never reached.
    let denied = fixture
        .service
        .observe(
            "observe-unauthorized",
            &fixture.run.run_id,
            fixture.run.version,
        )
        .await
        .expect_err("an unauthorized run cannot be observed");
    assert_eq!(denied.code, ComputerErrorCode::InvalidState);
    assert!(fixture.backend_calls().is_empty());

    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));
    fixture.arm_probe(&receipt.approval_id);

    // An issued-but-unspent receipt still buys no observation.
    let still_denied = fixture
        .service
        .observe(
            "observe-pre-consume",
            &fixture.run.run_id,
            fixture.run.version,
        )
        .await
        .expect_err("an unspent receipt cannot be observed against");
    assert_eq!(still_denied.code, ComputerErrorCode::InvalidState);
    assert!(
        fixture.backend_calls().is_empty(),
        "no frame capture may precede receipt consumption"
    );

    let run = fixture
        .authorize(
            "authorize-1",
            &approved,
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect("receipt authorizes control");
    assert!(fixture.backend_calls().is_empty());

    fixture
        .service
        .observe("observe-authorized", &run.run_id, run.version)
        .await
        .expect("an authorized run observes");

    let calls = fixture.backend_calls();
    assert_eq!(calls.len(), 1, "exactly one frame operation: {calls:?}");
    assert_eq!(calls[0].operation, "observe");
    assert_eq!(
        calls[0].approval_status_at_call, "Consumed",
        "the first backend operation must find the receipt already spent"
    );
}

#[test]
fn a_fabricated_or_missing_receipt_never_authorizes() {
    let fixture = Fixture::new();
    let requested = grant(&[ActionClass::Semantic], Some(1), 30_000);

    let fabricated = ApprovalPresentation {
        approval_id: Uuid::new_v4().to_string(),
        nonce: "b".repeat(64),
    };
    let error = fixture
        .authorize(
            "authorize-fabricated",
            &requested,
            &fabricated,
            &capability_revision(),
            at(1),
        )
        .expect_err("a fabricated receipt is refused");
    assert_eq!(error.code, ComputerErrorCode::Unauthorized);

    let malformed = ApprovalPresentation {
        approval_id: "   ".into(),
        nonce: "short".into(),
    };
    assert!(fixture
        .authorize(
            "authorize-malformed",
            &requested,
            &malformed,
            &capability_revision(),
            at(1)
        )
        .is_err());

    assert!(!fixture.granted());
    assert!(fixture.backend_calls().is_empty());
}

#[test]
fn a_pending_or_denied_approval_is_not_a_receipt() {
    for approve in [false, true] {
        let fixture = Fixture::new();
        let approved = grant(&[ActionClass::Semantic], Some(1), 30_000);
        let issued = fixture
            .service
            .request_control_approval(
                "req-1",
                fixture.owner,
                &fixture.workspace,
                &fixture.run.run_id,
                fixture.run.version,
                principal(),
                &capability_revision(),
                &approved,
                at(0),
            )
            .unwrap();
        let presentation = ApprovalPresentation {
            approval_id: issued.record.approval_id.clone(),
            nonce: issued.nonce,
        };
        if approve {
            // Explicitly refused by the human.
            fixture
                .service
                .decide_control_approval(
                    &presentation.approval_id,
                    fixture.owner,
                    false,
                    &capability_revision(),
                    at(0),
                )
                .unwrap();
        }
        let error = fixture
            .authorize(
                "authorize-1",
                &approved,
                &presentation,
                &capability_revision(),
                at(1),
            )
            .expect_err("an undecided or refused approval is not authority");
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
        assert!(!fixture.granted());
        assert!(fixture.backend_calls().is_empty());
    }
}

/// One presentation attempt with exactly one binding dimension changed.
struct Attempt {
    label: &'static str,
    principal: ApprovalPrincipal,
    owner: Option<Uuid>,
    workspace: &'static str,
    other_run: bool,
    forge_nonce: bool,
}

impl Attempt {
    fn base(label: &'static str) -> Self {
        Self {
            label,
            principal: principal(),
            owner: None,
            workspace: WORKSPACE,
            other_run: false,
            forge_nonce: false,
        }
    }
}

#[test]
fn every_binding_dimension_must_match_exactly() {
    let approved = grant(&[ActionClass::Semantic], Some(1), 30_000);

    let mut rotated_token = Attempt::base("rotated bearer token");
    rotated_token.principal.token_fingerprint = "c".repeat(64);

    let mut other_session = Attempt::base("different MCP transport session");
    other_session.principal.mcp_session_id = "mcp-session-beta".into();
    other_session.principal.client_actor_id = "coordinator@1.0#mcp-session-beta".into();

    let mut other_owner = Attempt::base("different owning session");
    other_owner.owner = Some(Uuid::new_v4());

    let mut other_workspace = Attempt::base("different workspace");
    other_workspace.workspace = OTHER_WORKSPACE;

    let mut other_run = Attempt::base("different run");
    other_run.other_run = true;

    let mut forged = Attempt::base("guessed nonce");
    forged.forge_nonce = true;

    for attempt in [
        rotated_token,
        other_session,
        other_owner,
        other_workspace,
        other_run,
        forged,
    ] {
        let label = attempt.label;
        let fixture = Fixture::new();
        let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));

        let target_run = if attempt.other_run {
            fixture
                .service
                .create_run(
                    "fixture-create-other",
                    fixture.owner,
                    Some(WORKSPACE.to_string()),
                    SimulatorBackend::demo_target(),
                    ComputerUseLimits::default(),
                )
                .unwrap()
        } else {
            fixture.run.clone()
        };
        let presentation = ApprovalPresentation {
            approval_id: receipt.approval_id.clone(),
            nonce: if attempt.forge_nonce {
                "d".repeat(64)
            } else {
                receipt.nonce.clone()
            },
        };

        let error = fixture
            .service
            .authorize_with_receipt(
                "authorize-1",
                attempt.owner.unwrap_or(fixture.owner),
                attempt.workspace,
                &target_run.run_id,
                target_run.version,
                attempt.principal,
                &capability_revision(),
                approved.clone(),
                &presentation,
                at(1),
            )
            .err()
            .unwrap_or_else(|| panic!("{label} must be refused"));

        assert_eq!(
            error.code,
            ComputerErrorCode::Unauthorized,
            "{label} must fail closed"
        );
        // A cross-scope caller is stopped by the run-ownership gate before the
        // ledger is consulted; a same-scope caller with the wrong principal or
        // nonce is stopped by the receipt gate. Both messages are fixed
        // constants shared by every failure in their class, so neither reveals
        // which dimension was wrong.
        assert!(
            [
                "computer use approval is not available to this caller",
                "computer run is not available to this session",
            ]
            .contains(&error.message.as_str()),
            "{label} produced a distinguishing message: {error:?}"
        );
        assert!(!fixture.granted(), "{label} must not grant control");
        assert!(
            fixture.backend_calls().is_empty(),
            "{label} reached the backend"
        );

        // A wrong caller must not be able to burn a legitimate approval.
        let projection = fixture
            .reader
            .read_control_approval(
                &receipt.approval_id,
                &principal(),
                fixture.owner,
                &fixture.workspace,
                at(1),
            )
            .unwrap();
        assert_eq!(projection.status, ApprovalStatus::Approved, "{label}");
    }
}

#[test]
fn an_expired_receipt_fails_closed() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(1), 30_000);
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));

    let error = fixture
        .authorize(
            "authorize-late",
            &approved,
            &receipt,
            &capability_revision(),
            at(6),
        )
        .expect_err("a receipt past its consumption window is refused");
    assert_eq!(error.code, ComputerErrorCode::InvalidState);
    assert!(error.message.contains("expired"));
    assert!(!fixture.granted());
    assert!(fixture.backend_calls().is_empty());

    let projection = fixture
        .reader
        .read_control_approval(
            &receipt.approval_id,
            &principal(),
            fixture.owner,
            &fixture.workspace,
            at(6),
        )
        .unwrap();
    assert_eq!(projection.status, ApprovalStatus::Expired);
}

#[test]
fn a_receipt_is_spent_exactly_once() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(1), 30_000);
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));

    fixture
        .authorize(
            "authorize-1",
            &approved,
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect("first use succeeds");

    // The same request id replays the stored result: that is idempotency, not
    // a second grant.
    let replay = fixture
        .authorize(
            "authorize-1",
            &approved,
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect("an identical retry replays");
    assert_eq!(replay.grant.as_ref().map(|g| g.grant_id.clone()), {
        fixture
            .service
            .get_run(&fixture.run.run_id)
            .unwrap()
            .unwrap()
            .grant
            .map(|g| g.grant_id)
    });

    // A fresh mutation presenting the spent receipt is a replay attack. It is
    // driven at the run's *current* revision so the version fence cannot mask
    // the receipt guard being asserted here.
    let current = fixture
        .service
        .get_run(&fixture.run.run_id)
        .unwrap()
        .unwrap();
    let error = fixture
        .service
        .authorize_with_receipt(
            "authorize-2",
            fixture.owner,
            &fixture.workspace,
            &current.run_id,
            current.version,
            principal(),
            &capability_revision(),
            approved.clone(),
            &receipt,
            at(1),
        )
        .expect_err("a consumed receipt cannot authorize again");
    assert_eq!(error.code, ComputerErrorCode::Conflict);
    assert!(
        error.message.contains("already consumed"),
        "unexpected replay error: {error:?}"
    );
}

#[test]
fn a_stale_capability_revision_invalidates_the_receipt() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(1), 30_000);

    // The human decided while a different capability set was advertised.
    let mut ungated = advertised_capabilities();
    for capability in &mut ungated.capabilities {
        if capability.id == "computer.control" {
            capability.human_gate = false;
            capability.availability = CapabilityAvailability::Available;
        }
    }
    let stale = capability_revision_of(&ungated);
    assert_ne!(stale, capability_revision());

    let receipt = fixture.issued_receipt("req-1", &approved, &stale, at(0));
    let error = fixture
        .authorize(
            "authorize-1",
            &approved,
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect_err("a receipt issued against another contract revision is refused");
    assert_eq!(error.code, ComputerErrorCode::InvalidState);
    assert!(error.message.contains("stale capability revision"));
    assert!(!fixture.granted());
    assert!(fixture.backend_calls().is_empty());
}

#[test]
fn over_broad_requests_are_refused_and_narrow_ones_accepted() {
    let approved = grant(&[ActionClass::Semantic], Some(2), 30_000);
    let over_broad = [
        (
            "extra action class",
            grant(
                &[ActionClass::Semantic, ActionClass::TextEntry],
                Some(2),
                30_000,
            ),
        ),
        (
            "more uses",
            grant(&[ActionClass::Semantic], Some(3), 30_000),
        ),
        (
            "longer lease",
            grant(&[ActionClass::Semantic], Some(2), 30_001),
        ),
        (
            "unbounded uses",
            grant(&[ActionClass::Semantic], None, 30_000),
        ),
    ];
    for (label, requested) in over_broad {
        let fixture = Fixture::new();
        let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));
        let error = fixture
            .authorize(
                "authorize-1",
                &requested,
                &receipt,
                &capability_revision(),
                at(1),
            )
            .expect_err(label);
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction, "{label}");
        assert!(!fixture.granted(), "{label} must not grant control");
        assert!(fixture.backend_calls().is_empty(), "{label}");
    }

    let fixture = Fixture::new();
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));
    let run = fixture
        .authorize(
            "authorize-1",
            &grant(&[ActionClass::Semantic], Some(1), 1_000),
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect("a strictly narrower request is accepted");
    assert_eq!(run.grant.unwrap().uses_remaining, Some(1));
}

#[test]
fn concurrent_consumers_of_one_receipt_produce_exactly_one_grant() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(2), 30_000);
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));

    const RACERS: usize = 8;
    let barrier = Barrier::new(RACERS);
    let outcomes: Vec<Result<ComputerRun, ComputerError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..RACERS)
            .map(|index| {
                let fixture = &fixture;
                let receipt = &receipt;
                let approved = &approved;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    fixture.authorize(
                        &format!("authorize-race-{index}"),
                        approved,
                        receipt,
                        &capability_revision(),
                        at(1),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });

    let winners: Vec<_> = outcomes.iter().filter(|outcome| outcome.is_ok()).collect();
    assert_eq!(winners.len(), 1, "exactly one racer may spend the receipt");
    for outcome in outcomes.iter().filter(|outcome| outcome.is_err()) {
        let error = outcome.as_ref().unwrap_err();
        assert_eq!(
            error.code,
            ComputerErrorCode::Conflict,
            "every loser must fail closed: {error:?}"
        );
    }

    let projection = fixture
        .reader
        .read_control_approval(
            &receipt.approval_id,
            &principal(),
            fixture.owner,
            &fixture.workspace,
            at(1),
        )
        .unwrap();
    assert_eq!(projection.status, ApprovalStatus::Consumed);
    assert_eq!(projection.consumed_at, Some(at(1)));
    assert!(fixture.backend_calls().is_empty());
}

#[test]
fn restart_preserves_consumption_and_revokes_live_authority() {
    let dir = tempdir().unwrap();
    let owner = Uuid::new_v4();
    let (spent_id, spent_nonce, live_id, live_nonce, run_id);

    {
        let store = ComputerStore::open(dir.path()).unwrap();
        let service = Arc::new(ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            store.clone(),
        ));
        let run = service
            .create_run(
                "create",
                owner,
                Some(WORKSPACE.to_string()),
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        run_id = run.run_id.clone();
        let approved = grant(&[ActionClass::Semantic], Some(2), 30_000);

        let spent = service
            .request_control_approval(
                "req-spent",
                owner,
                WORKSPACE,
                &run.run_id,
                run.version,
                principal(),
                &capability_revision(),
                &approved,
                at(0),
            )
            .unwrap();
        let live = service
            .request_control_approval(
                "req-live",
                owner,
                WORKSPACE,
                &run.run_id,
                run.version,
                principal(),
                &capability_revision(),
                &approved,
                at(0),
            )
            .unwrap();
        for id in [&spent.record.approval_id, &live.record.approval_id] {
            service
                .decide_control_approval(id, owner, true, &capability_revision(), at(0))
                .unwrap();
        }
        spent_id = spent.record.approval_id.clone();
        spent_nonce = spent.nonce.clone();
        live_id = live.record.approval_id.clone();
        live_nonce = live.nonce.clone();

        service
            .authorize_with_receipt(
                "authorize-1",
                owner,
                WORKSPACE,
                &run.run_id,
                run.version,
                principal(),
                &capability_revision(),
                approved,
                &ApprovalPresentation {
                    approval_id: spent_id.clone(),
                    nonce: spent_nonce.clone(),
                },
                at(1),
            )
            .expect("receipt is spent before restart");
    }

    // Restart: a fresh process opens the same durable ledger.
    let store = ComputerStore::open(dir.path()).unwrap();
    let service = Arc::new(ComputerUseService::new(
        Arc::new(SimulatorBackend::new()),
        store,
    ));

    let spent = service
        .read_control_approval(&spent_id, &principal(), owner, WORKSPACE, at(2))
        .unwrap();
    assert_eq!(
        spent.status,
        ApprovalStatus::Consumed,
        "restart must not resurrect a spent receipt"
    );

    let live = service
        .read_control_approval(&live_id, &principal(), owner, WORKSPACE, at(2))
        .unwrap();
    assert_eq!(
        live.status,
        ApprovalStatus::Revoked,
        "restart must invalidate approvals the human granted against a run that no longer exists"
    );

    // Neither survives as authority. The run itself is interrupted by restart
    // recovery, so both presentations fail closed.
    for (id, nonce) in [(&spent_id, &spent_nonce), (&live_id, &live_nonce)] {
        let error = service
            .authorize_with_receipt(
                &format!("authorize-after-restart-{id}"),
                owner,
                WORKSPACE,
                &run_id,
                1,
                principal(),
                &capability_revision(),
                grant(&[ActionClass::Semantic], Some(1), 30_000),
                &ApprovalPresentation {
                    approval_id: id.clone(),
                    nonce: nonce.clone(),
                },
                at(2),
            )
            .expect_err("no receipt survives a restart as usable authority");
        assert!(matches!(
            error.code,
            ComputerErrorCode::Conflict
                | ComputerErrorCode::InvalidState
                | ComputerErrorCode::Unauthorized
        ));
    }
}

#[test]
fn de_escalating_controls_revoke_outstanding_receipts() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(2), 30_000);
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));

    let revoked = fixture
        .service
        .revoke_control_approvals_for_run(&fixture.run.run_id)
        .unwrap();
    assert_eq!(revoked, 1);

    let error = fixture
        .authorize(
            "authorize-1",
            &approved,
            &receipt,
            &capability_revision(),
            at(1),
        )
        .expect_err("a revoked receipt is not authority");
    assert_eq!(error.code, ComputerErrorCode::InvalidState);
    assert!(!fixture.granted());
}

#[test]
fn the_public_projection_carries_no_secret_or_transport_identity() {
    let fixture = Fixture::new();
    let approved = grant(
        &[ActionClass::Semantic, ActionClass::TextEntry],
        Some(3),
        45_000,
    );
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));

    let projection: ApprovalProjection = fixture
        .reader
        .read_control_approval(
            &receipt.approval_id,
            &principal(),
            fixture.owner,
            &fixture.workspace,
            at(0),
        )
        .unwrap();
    let encoded = serde_json::to_string(&projection).unwrap();

    let principal = principal();
    for secret in [
        receipt.nonce.as_str(),
        principal.token_fingerprint.as_str(),
        principal.mcp_session_id.as_str(),
        principal.client_actor_id.as_str(),
        fixture.workspace.as_str(),
        "req-1",
    ] {
        assert!(
            !encoded.contains(secret),
            "redacted approval projection leaked {secret}: {encoded}"
        );
    }
    // What it *does* carry is exactly the human-meaningful decision surface.
    let value: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["status"], "approved");
    assert_eq!(value["capabilityId"], "computer.control");
    assert_eq!(value["maxUses"], 3);
    assert_eq!(value["maxTtlMs"], 45_000);
    assert_eq!(value["actionClasses"], json!(["semantic", "text_entry"]));
}

#[test]
fn the_durable_ledger_never_stores_the_nonce() {
    let fixture = Fixture::new();
    let approved = grant(&[ActionClass::Semantic], Some(1), 30_000);
    let receipt = fixture.issued_receipt("req-1", &approved, &capability_revision(), at(0));
    drop(fixture.store.clone());

    let approvals = fixture._dir.path().join("approvals");
    let mut found = false;
    for entry in std::fs::read_dir(&approvals).unwrap() {
        let bytes = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(
            !bytes.contains(&receipt.nonce),
            "the durable ledger must store only a nonce digest"
        );
        found = true;
    }
    assert!(found, "the approval was persisted");
}

// ── MCP transport boundary ───────────────────────────────────────────────

struct ReceiptController {
    service: Arc<ComputerUseService>,
}

impl ReceiptController {
    fn scoped(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
    ) -> Result<ComputerRun, ComputerError> {
        self.service
            .get_run(run_id)?
            .filter(|run| {
                run.owner_session_id == owner_session_id
                    && run.workspace.as_deref() == Some(workspace)
            })
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "computer run is not available to this client scope",
                )
            })
    }
}

#[async_trait]
impl ComputerRunController for ReceiptController {
    #[allow(clippy::too_many_arguments)]
    async fn request_approval(
        &self,
        principal: &ApprovalPrincipal,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        capability_revision: &str,
        grant_request: ComputerGrantRequest,
    ) -> Result<IssuedApproval, ComputerError> {
        self.scoped(owner_session_id, workspace, run_id)?;
        let now = Utc::now();
        let issued = self.service.request_control_approval(
            request_id,
            owner_session_id,
            workspace,
            run_id,
            expected_version,
            principal.clone(),
            capability_revision,
            &grant_request,
            now,
        )?;
        Ok(IssuedApproval {
            approval: issued.record.project_at(now),
            nonce: issued.nonce,
        })
    }

    async fn read_approval(
        &self,
        principal: &ApprovalPrincipal,
        owner_session_id: Uuid,
        workspace: &str,
        approval_id: &str,
    ) -> Result<ApprovalProjection, ComputerError> {
        self.service.read_control_approval(
            approval_id,
            principal,
            owner_session_id,
            workspace,
            Utc::now(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn authorize(
        &self,
        principal: &ApprovalPrincipal,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        capability_revision: &str,
        grant_request: ComputerGrantRequest,
        presentation: &ApprovalPresentation,
    ) -> Result<ComputerRun, ComputerError> {
        self.service.authorize_with_receipt(
            request_id,
            owner_session_id,
            workspace,
            run_id,
            expected_version,
            principal.clone(),
            capability_revision,
            grant_request,
            presentation,
            Utc::now(),
        )
    }

    async fn pause(
        &self,
        _client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.scoped(owner_session_id, workspace, run_id)?;
        self.service.revoke_control_approvals_for_run(run_id)?;
        self.service
            .pause(request_id, run_id, expected_version)
            .await
    }

    async fn take_over(
        &self,
        _client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.scoped(owner_session_id, workspace, run_id)?;
        self.service.revoke_control_approvals_for_run(run_id)?;
        self.service
            .take_over(request_id, run_id, expected_version)
            .await
    }

    async fn cancel(
        &self,
        _client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        _expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.scoped(owner_session_id, workspace, run_id)?;
        self.service.revoke_control_approvals_for_run(run_id)?;
        self.service.cancel(request_id, run_id).await
    }
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
    session: Option<&str>,
    id: u64,
    method: &str,
    params: Value,
) -> reqwest::Response {
    let mut request = client
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    if let Some(bearer) = bearer {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    request.send().await.unwrap()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn transport_auth_and_mcp_session_alone_never_grant_computer_control() {
    let _guard = home_override_serial();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let session = host
        .session_new_kind(grokptah_agent_bridge::SessionKind::Build)
        .unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();

    let store = host.ensure_computer_store().unwrap();
    let (backend, calls, _probe) = RecordingBackend::new();
    let service = Arc::new(ComputerUseService::new(backend, store));
    let workspace_string = canonical_workspace_string(workspace.path()).unwrap();
    let run = service
        .create_run(
            "fixture-create",
            session.id,
            Some(workspace_string),
            SimulatorBackend::demo_target(),
            ComputerUseLimits::default(),
        )
        .unwrap();
    host.set_computer_run_controller(Arc::new(ReceiptController {
        service: service.clone(),
    }));

    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "receipt-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let url = format!("http://{}/mcp", server.addr);
    let client = reqwest::Client::new();

    // 1. Unauthenticated: no bearer token at all.
    let unauthenticated = rpc(
        &client,
        &url,
        None,
        None,
        1,
        "initialize",
        json!({"protocolVersion":"2025-11-25","capabilities":{},
               "clientInfo":{"name":"receipt-client","version":"1.0"}}),
    )
    .await;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let init = rpc(
        &client,
        &url,
        Some("receipt-token"),
        None,
        2,
        "initialize",
        json!({"protocolVersion":"2025-11-25","capabilities":{},
               "clientInfo":{"name":"receipt-client","version":"1.0"}}),
    )
    .await;
    assert_eq!(init.status(), reqwest::StatusCode::OK);
    let transport_session = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let _ = rpc(
        &client,
        &url,
        Some("receipt-token"),
        Some(&transport_session),
        3,
        "notifications/initialized",
        json!({}),
    )
    .await;

    // The advertised contract still promises a human gate, and now also
    // advertises the request/read pair a consumer needs to satisfy it.
    let capabilities: Value = serde_json::to_value(advertised_capabilities()).unwrap();
    let control = capabilities["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["id"] == "computer.control")
        .expect("computer.control is advertised");
    assert_eq!(control["human_gate"], true);
    assert_eq!(control["availability"], "gated");

    let authorize_args = |extra: Value| {
        let mut args = json!({
            "request_id":"mcp-authorize-1",
            "session_id":session.id,
            "workspace":workspace.path(),
            "run_id":run.run_id,
            "expected_version":run.version,
            "action_classes":["semantic"],
            "ttl_ms":60000,
            "uses_remaining":1
        });
        if let (Some(target), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        args
    };

    // 2. Authenticated + initialized MCP session, no receipt at all: the
    //    schema itself refuses before any authorization code runs.
    let no_receipt = rpc(
        &client,
        &url,
        Some("receipt-token"),
        Some(&transport_session),
        4,
        "tools/call",
        json!({"name":"ptah_authorize_computer_run","arguments":authorize_args(json!({}))}),
    )
    .await;
    assert_ne!(
        no_receipt.status(),
        reqwest::StatusCode::OK,
        "an initialized MCP session is not a human approval"
    );

    // 3. Authenticated + initialized + a fabricated receipt.
    let fabricated = rpc(
        &client,
        &url,
        Some("receipt-token"),
        Some(&transport_session),
        5,
        "tools/call",
        json!({"name":"ptah_authorize_computer_run","arguments":authorize_args(json!({
            "approval_id": Uuid::new_v4().to_string(),
            "approval_nonce": "e".repeat(64),
        }))}),
    )
    .await;
    assert_ne!(fabricated.status(), reqwest::StatusCode::OK);

    assert!(
        service
            .get_run(&run.run_id)
            .unwrap()
            .unwrap()
            .grant
            .is_none(),
        "no un-receipted path may attach control authority"
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "no frame or input operation may occur without a spent receipt"
    );

    server.stop();
    set_grokptah_home_override(None);
}
