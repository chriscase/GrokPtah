//! Adversarial coverage for the provider capability generation (#458).
//!
//! Every test here uses a synthetic provider: a plain struct holding the
//! capability facts a real provider route would produce. Nothing in this file
//! resolves a credential, opens a socket, or reaches a provider. The point is
//! the *timing* — a capability that changes between one boundary and the next
//! — and that is reproducible only when the provider is a value the test can
//! move at an exact instant.
//!
//! The shape of each race is the same: qualify, pass some boundaries, change
//! the capability, then present the same binding at the next boundary. The
//! assertion is always that the binding stops working and, where a physical
//! action was in flight, that the backend was never touched.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use parking_lot::Mutex;
use tempfile::TempDir;
use uuid::Uuid;

use grokptah_agent_bridge::capability_authority::{
    AssuranceProfile, CapabilityAssessment, CapabilityBindingRef, CapabilityBoundary,
    CapabilityDenied, CapabilityRegistry, CapabilityRequest, DeclaredCapabilityPolicy,
    NormalizedRoute, QualificationEvidence, QualificationEvidenceKind, QualificationKey,
    QualificationSchema,
};
use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend, ComputerCapabilities,
    ComputerCapabilityGate, ComputerError, ComputerErrorCode, ComputerObservation, ComputerRun,
    ComputerStore, ComputerTarget, ComputerUseLimits, GrantIssuer, ObservationGeometry,
    SemanticAction, SemanticElement, Sensitivity,
};
use grokptah_agent_bridge::{CapabilitySource, ComputerUseService, ComputerUseTier};

const SCHEMA_ID: &str = "grokptah.computer-use.session-qualification";
const PROVIDER: &str = "synthetic";
const SELECTION: &str = "synthetic/model-a";
const PRINCIPAL_A: &str = "v1-sha256:synthetic-principal-a";
const PRINCIPAL_B: &str = "v1-sha256:synthetic-principal-b";

/// The capability facts a synthetic provider route currently reports.
///
/// Tests move this between boundaries. Nothing else about the run changes, so
/// a refusal can only come from the capability having moved.
#[derive(Debug)]
struct SyntheticProvider {
    facts: Mutex<ProviderFacts>,
}

#[derive(Debug, Clone)]
struct ProviderFacts {
    base_url: String,
    wire_model: String,
    dialect: String,
    selection_key: String,
    source: CapabilitySource,
    tier: ComputerUseTier,
    fingerprint: String,
}

impl SyntheticProvider {
    fn measured_semantic() -> Arc<Self> {
        Arc::new(Self {
            facts: Mutex::new(ProviderFacts {
                base_url: "https://synthetic.invalid/v1".into(),
                wire_model: "model-a".into(),
                dialect: "SyntheticChatCompletions".into(),
                selection_key: SELECTION.into(),
                source: CapabilitySource::Measured,
                tier: ComputerUseTier::SemanticAct,
                fingerprint: PRINCIPAL_A.into(),
            }),
        })
    }

    fn request(&self) -> CapabilityRequest {
        let facts = self.facts.lock().clone();
        CapabilityRequest {
            route: NormalizedRoute::new(PROVIDER, &facts.base_url, facts.wire_model, facts.dialect),
            selection_key: facts.selection_key,
            source: facts.source,
            claimed_tier: facts.tier,
            credential_fingerprint: facts.fingerprint,
        }
    }

    fn downgrade_to_observe(&self) {
        self.facts.lock().tier = ComputerUseTier::Observe;
    }

    fn become_declared(&self) {
        self.facts.lock().source = CapabilitySource::Declared;
    }

    fn rotate_credential(&self) {
        self.facts.lock().fingerprint = PRINCIPAL_B.into();
    }

    fn retarget_model(&self) {
        let mut facts = self.facts.lock();
        facts.wire_model = "model-b".into();
    }
}

/// A live capability authority wired to a synthetic provider.
///
/// This is the same shape the desktop host installs: the kernel asks, and the
/// gate answers by re-deriving the capability from scratch at that instant.
#[derive(Debug)]
struct SyntheticGate {
    registry: Arc<CapabilityRegistry>,
    provider: Arc<SyntheticProvider>,
    session_id: Uuid,
    checks: Mutex<Vec<CapabilityBoundary>>,
}

impl SyntheticGate {
    fn new(
        registry: Arc<CapabilityRegistry>,
        provider: Arc<SyntheticProvider>,
        session_id: Uuid,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            provider,
            session_id,
            checks: Mutex::new(Vec::new()),
        })
    }

    fn checked(&self) -> Vec<CapabilityBoundary> {
        self.checks.lock().clone()
    }
}

impl ComputerCapabilityGate for SyntheticGate {
    fn authorize(
        &self,
        boundary: CapabilityBoundary,
        owner_session_id: Uuid,
        binding: Option<&CapabilityBindingRef>,
    ) -> Result<(), ComputerError> {
        let Some(binding) = binding else {
            return Ok(());
        };
        self.checks.lock().push(boundary);
        let live =
            live_assessment(&self.registry, &self.provider).map_err(|_| capability_error())?;
        self.registry
            .validate(owner_session_id, binding, boundary, &live)
            .map_err(|_| capability_error())?;
        assert_eq!(
            owner_session_id, self.session_id,
            "the kernel must present the owning session"
        );
        Ok(())
    }
}

fn capability_error() -> ComputerError {
    ComputerError::new(ComputerErrorCode::Unauthorized, CapabilityDenied::MESSAGE)
}

/// Re-derives live capability facts exactly as the host does: observe the
/// credential first, then assess. Observing first is what makes a rotation
/// visible at the boundary rather than at the next qualification.
fn live_assessment(
    registry: &CapabilityRegistry,
    provider: &SyntheticProvider,
) -> Result<CapabilityAssessment, CapabilityDenied> {
    let request = provider.request();
    registry.observe_credential(PROVIDER, &request.credential_fingerprint)?;
    registry.assess(&request)
}

fn registry(
    profile: AssuranceProfile,
    declared: DeclaredCapabilityPolicy,
) -> Arc<CapabilityRegistry> {
    Arc::new(CapabilityRegistry::new(
        profile,
        declared,
        QualificationSchema::new(SCHEMA_ID, 1),
    ))
}

fn evidence_for(profile: AssuranceProfile) -> QualificationEvidence {
    let kind = if profile == AssuranceProfile::HighAssurance {
        QualificationEvidenceKind::Signed
    } else {
        QualificationEvidenceKind::Measured
    };
    QualificationEvidence::of(kind, b"synthetic-qualification-transcript")
}

fn try_qualify(
    registry: &CapabilityRegistry,
    provider: &SyntheticProvider,
    session_id: Uuid,
    profile: AssuranceProfile,
) -> Result<CapabilityBindingRef, CapabilityDenied> {
    let key = QualificationKey::new(session_id, SELECTION);
    let assessment = live_assessment(registry, provider)?;
    registry.qualify(&key, &assessment, &evidence_for(profile))
}

fn qualify(
    registry: &CapabilityRegistry,
    provider: &SyntheticProvider,
    session_id: Uuid,
    profile: AssuranceProfile,
) -> CapabilityBindingRef {
    try_qualify(registry, provider, session_id, profile).expect("qualify")
}

/// A backend that records whether a physical action ever reached it.
#[derive(Debug, Default)]
struct CountingBackend {
    observations: AtomicUsize,
    dispatches: AtomicUsize,
}

impl CountingBackend {
    fn dispatches(&self) -> usize {
        self.dispatches.load(Ordering::SeqCst)
    }

    fn observations(&self) -> usize {
        self.observations.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ComputerBackend for CountingBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "synthetic_capability_fixture".into(),
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
        limits: &ComputerUseLimits,
    ) -> Result<ComputerObservation, ComputerError> {
        let sequence = self.observations.fetch_add(1, Ordering::SeqCst) as u64 + 1;
        let observation = ComputerObservation {
            observation_id: observation_id.into(),
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
                element_id: "name-field".into(),
                role: "text_field".into(),
                label: Some("Name".into()),
                value: None,
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        };
        observation.validate(limits)?;
        Ok(observation)
    }

    async fn act(
        &self,
        _run_id: &str,
        _observation: &ComputerObservation,
        _action: &ComputerAction,
    ) -> Result<ActionOutcome, ComputerError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutcome::bounded("synthetic action", Some(true)))
    }

    async fn cancel(&self, _run_id: &str) -> Result<(), ComputerError> {
        Ok(())
    }
}

fn target() -> ComputerTarget {
    ComputerTarget {
        app_id: "com.grokptah.capability-generation-fixture".into(),
        window_id: "main-window".into(),
        generation: 1,
        display_name: "Synthetic capability fixture".into(),
        sensitivity: Sensitivity::None,
    }
}

fn grant(run: &ComputerRun) -> ActionGrant {
    let issued_at = Utc::now() - Duration::seconds(1);
    ActionGrant {
        grant_id: format!("grant-{}", run.run_id),
        run_id: run.run_id.clone(),
        target: run.target.clone(),
        action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        issued_by: GrantIssuer::LocalUser,
        issued_at,
        expires_at: issued_at + Duration::minutes(5),
        uses_remaining: Some(8),
        revoked_at: None,
    }
}

struct Fixture {
    _directory: TempDir,
    backend: Arc<CountingBackend>,
    service: ComputerUseService,
    registry: Arc<CapabilityRegistry>,
    provider: Arc<SyntheticProvider>,
    gate: Arc<SyntheticGate>,
    session_id: Uuid,
    run_id: String,
    profile: AssuranceProfile,
}

impl Fixture {
    /// Builds a run that is authorized, observed, and staged under a live
    /// model authority — the state a cockpit is in the instant before an
    /// operator approves a model's proposal.
    async fn staged(profile: AssuranceProfile, declared: DeclaredCapabilityPolicy) -> Self {
        let fixture = Self::ready(profile, declared).await;
        let binding = qualify(
            &fixture.registry,
            &fixture.provider,
            fixture.session_id,
            fixture.profile,
        );
        let run = fixture.run();
        fixture
            .service
            .bind_model_authority("stage", &fixture.run_id, run.version, binding)
            .expect("stage model authority");
        fixture
            .observe()
            .await
            .expect("observe under model authority");
        fixture
    }

    /// Builds an authorized run with no model authority attached yet.
    async fn ready(profile: AssuranceProfile, declared: DeclaredCapabilityPolicy) -> Self {
        let directory = tempfile::tempdir().expect("fixture directory");
        let backend = Arc::new(CountingBackend::default());
        let store = ComputerStore::open(directory.path().join("computer-use")).expect("store");
        let registry = registry(profile, declared);
        let provider = SyntheticProvider::measured_semantic();
        let session_id = Uuid::new_v4();
        let gate = SyntheticGate::new(registry.clone(), provider.clone(), session_id);
        let service =
            ComputerUseService::new(backend.clone(), store).with_capability_gate(gate.clone());
        let run = service
            .create_run(
                "create",
                session_id,
                None,
                target(),
                ComputerUseLimits::default(),
            )
            .expect("create run");
        let run = service
            .authorize("authorize", &run.run_id, run.version, grant(&run))
            .expect("authorize run");
        Self {
            _directory: directory,
            backend,
            service,
            registry,
            provider,
            gate,
            session_id,
            run_id: run.run_id,
            profile,
        }
    }

    fn run(&self) -> ComputerRun {
        self.service
            .get_run(&self.run_id)
            .expect("load run")
            .expect("run exists")
    }

    async fn observe(&self) -> Result<ComputerObservation, ComputerError> {
        let version = self.run().version;
        self.service
            .observe(&request_id(), &self.run_id, version)
            .await
    }

    async fn dispatch(&self) -> Result<ActionOutcome, ComputerError> {
        let run = self.run();
        let observation_id = run
            .current_observation
            .as_ref()
            .map(|observation| observation.observation_id.clone())
            .expect("run has a current observation");
        self.service
            .act(
                &request_id(),
                &self.run_id,
                run.version,
                &observation_id,
                ComputerAction::SetValue {
                    element_id: "name-field".into(),
                    text: "PTAH_VISIBLE_DEMO_VALUE_V1".into(),
                },
            )
            .await
    }

    /// Re-leases action authority the way a reauthorized paused run does.
    fn lease(&self) -> Result<ComputerRun, ComputerError> {
        let run = self.run();
        self.service
            .authorize(&request_id(), &self.run_id, run.version, grant(&run))
    }

    fn restage(&self) -> Result<ComputerRun, ComputerError> {
        let binding = try_qualify(
            &self.registry,
            &self.provider,
            self.session_id,
            self.profile,
        )
        .map_err(|_| capability_error())?;
        let run = self.run();
        self.service
            .bind_model_authority(&request_id(), &self.run_id, run.version, binding)
    }
}

fn request_id() -> String {
    Uuid::new_v4().to_string()
}

fn assert_capability_denial(error: &ComputerError) {
    assert_eq!(error.code, ComputerErrorCode::Unauthorized);
    assert_eq!(error.message, CapabilityDenied::MESSAGE);
}

// ---------------------------------------------------------------------------
// Negative controls. Without these, every assertion below could pass because
// nothing ever works.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unchanged_capability_observes_and_dispatches() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    fixture
        .dispatch()
        .await
        .expect("a current capability dispatches");
    assert_eq!(fixture.backend.dispatches(), 1);
    assert!(fixture
        .gate
        .checked()
        .contains(&CapabilityBoundary::Dispatch));
    assert!(fixture
        .gate
        .checked()
        .contains(&CapabilityBoundary::LiveFrame));
}

#[tokio::test]
async fn an_operator_driven_run_needs_no_provider_capability() {
    let fixture = Fixture::ready(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    fixture.observe().await.expect("operator observation");
    fixture.dispatch().await.expect("operator dispatch");
    assert_eq!(fixture.backend.dispatches(), 1);
    assert!(
        fixture.gate.checked().is_empty(),
        "a run with no model authority must not consult the capability authority"
    );
}

#[tokio::test]
async fn a_kernel_with_no_capability_authority_refuses_model_authority() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let backend = Arc::new(CountingBackend::default());
    let store = ComputerStore::open(directory.path().join("computer-use")).expect("store");
    // Deliberately *not* wired to an authority.
    let service = ComputerUseService::new(backend.clone(), store);
    let session_id = Uuid::new_v4();
    let run = service
        .create_run(
            "create",
            session_id,
            None,
            target(),
            ComputerUseLimits::default(),
        )
        .expect("create run");
    let run = service
        .authorize("authorize", &run.run_id, run.version, grant(&run))
        .expect("authorize run");
    let error = service
        .bind_model_authority(
            "stage",
            &run.run_id,
            run.version,
            CapabilityBindingRef::unbound(),
        )
        .expect_err("an unwired kernel must not accept model authority");
    assert_capability_denial(&error);
    assert_eq!(backend.dispatches(), 0);
}

// ---------------------------------------------------------------------------
// Races between adjacent boundaries.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_downgrade_between_observation_and_dispatch_never_reaches_the_backend() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    assert_eq!(fixture.backend.observations(), 1);

    // The capability tier drops after the frame was delivered and before the
    // operator's approved action becomes physical.
    fixture.provider.downgrade_to_observe();

    let error = fixture
        .dispatch()
        .await
        .expect_err("a downgraded capability must not dispatch");
    assert_capability_denial(&error);
    assert_eq!(
        fixture.backend.dispatches(),
        0,
        "the check must sit before the backend, not after it"
    );
}

#[tokio::test]
async fn a_revocation_between_frames_stops_the_next_frame() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    assert_eq!(fixture.backend.observations(), 1);

    fixture.registry.revoke_all().expect("revoke");

    let error = fixture
        .observe()
        .await
        .expect_err("a revoked capability must not deliver another frame");
    assert_capability_denial(&error);
    assert_eq!(
        fixture.backend.observations(),
        1,
        "no further screen content may be captured under a revoked capability"
    );
}

#[tokio::test]
async fn a_credential_rotation_between_staging_and_dispatch_is_refused() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    fixture.provider.rotate_credential();
    let error = fixture
        .dispatch()
        .await
        .expect_err("a rotated credential is a different principal");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);
}

#[tokio::test]
async fn a_route_change_between_staging_and_dispatch_is_refused() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    fixture.provider.retarget_model();
    let error = fixture
        .dispatch()
        .await
        .expect_err("a retargeted wire model is a different route");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);
}

#[tokio::test]
async fn a_policy_change_between_approval_and_lease_is_refused() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    // Pause returns the run to a state a reauthorization can lease again.
    let run = fixture.run();
    fixture
        .service
        .pause(&request_id(), &fixture.run_id, run.version)
        .await
        .expect("pause");
    fixture.restage().expect("restage under current capability");

    fixture
        .registry
        .bump_policy_revision()
        .expect("operator policy change");

    let error = fixture
        .lease()
        .expect_err("a lease must not be taken out on a superseded policy revision");
    assert_capability_denial(&error);
}

#[tokio::test]
async fn a_schema_bump_between_staging_and_dispatch_is_refused() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    fixture
        .registry
        .set_qualification_schema(QualificationSchema::new(SCHEMA_ID, 2))
        .expect("schema drift");
    let error = fixture
        .dispatch()
        .await
        .expect_err("a qualification taken under an older schema proves nothing now");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);
}

#[tokio::test]
async fn a_failed_requalification_stops_the_next_dispatch() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    let key = QualificationKey::new(fixture.session_id, SELECTION);
    fixture
        .registry
        .record_requalification_failure(&key)
        .expect("record failure");
    let error = fixture
        .dispatch()
        .await
        .expect_err("a model that just failed to re-prove itself has no authority");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);
}

// ---------------------------------------------------------------------------
// Provenance.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn declared_capability_is_observation_only_without_configured_trust() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    // The provider's capability record is rewritten from measured to declared
    // without the route moving at all — exactly the case a route fingerprint
    // cannot see.
    fixture.provider.become_declared();

    let live = live_assessment(&fixture.registry, &fixture.provider).expect("assess");
    assert_eq!(live.tier(), ComputerUseTier::Observe);
    assert_eq!(live.provenance().label(), "declared_observation_only");
    assert!(!live.provenance().admits_action());

    let error = fixture
        .dispatch()
        .await
        .expect_err("a declared capability must not act");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);
}

#[tokio::test]
async fn declared_capability_acts_only_under_an_explicit_named_trust_policy() {
    let trusted = DeclaredCapabilityPolicy::trusted("operator-manifest").expect("trust policy");
    let fixture = Fixture::ready(AssuranceProfile::Balanced, trusted).await;
    fixture.provider.become_declared();

    let live = live_assessment(&fixture.registry, &fixture.provider).expect("assess");
    assert_eq!(live.tier(), ComputerUseTier::SemanticAct);
    assert_eq!(live.provenance().label(), "declared_trusted");
    assert_eq!(
        live.provenance().provenance_id(),
        Some("operator-manifest"),
        "the trusted source must be published, not implied"
    );

    let key = QualificationKey::new(fixture.session_id, SELECTION);
    let binding = fixture
        .registry
        .qualify(
            &key,
            &live,
            &QualificationEvidence::of(QualificationEvidenceKind::Declared, b"operator-manifest"),
        )
        .expect("explicit trust qualifies");
    let run = fixture.run();
    fixture
        .service
        .bind_model_authority(&request_id(), &fixture.run_id, run.version, binding)
        .expect("stage under trusted declaration");
    fixture.observe().await.expect("observe");
    fixture.dispatch().await.expect("trusted declaration acts");
    assert_eq!(fixture.backend.dispatches(), 1);

    // Withdrawing the trust invalidates it at the next boundary.
    fixture
        .registry
        .set_declared_policy(DeclaredCapabilityPolicy::ObservationOnly)
        .expect("withdraw trust");
    let error = fixture
        .observe()
        .await
        .expect_err("withdrawn trust must stop the next frame");
    assert_capability_denial(&error);
}

#[tokio::test]
async fn high_assurance_refuses_declared_trust_the_operator_configured() {
    let trusted = DeclaredCapabilityPolicy::trusted("operator-manifest").expect("trust policy");
    let fixture = Fixture::ready(AssuranceProfile::HighAssurance, trusted).await;
    fixture.provider.become_declared();
    let live = live_assessment(&fixture.registry, &fixture.provider).expect("assess");
    assert_eq!(
        live.tier(),
        ComputerUseTier::Observe,
        "the strictest profile does not honour declared trust"
    );
}

// ---------------------------------------------------------------------------
// Reincarnation, restart, exhaustion.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_binding_from_a_previous_process_names_nothing() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    let persisted = fixture
        .run()
        .capability_binding
        .expect("the staged run persists its binding reference");

    // A restart draws a fresh authority. The persisted reference survives on
    // disk; the binding it names does not.
    let restarted = registry(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    );
    let live = live_assessment(&restarted, &fixture.provider).expect("assess");
    assert_eq!(
        restarted.validate(
            fixture.session_id,
            &persisted,
            CapabilityBoundary::Dispatch,
            &live
        ),
        Err(CapabilityDenied)
    );

    let key = QualificationKey::new(fixture.session_id, SELECTION);
    restarted.quarantine_if_unbound(&key, &persisted);
    assert!(
        restarted.is_quarantined(&key),
        "an unbound qualification must be quarantined, not treated as stale"
    );
}

#[tokio::test]
async fn deleting_and_re_adding_the_same_credential_does_not_restore_authority() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    let before = fixture.run().capability_binding.expect("staged binding");

    fixture
        .registry
        .forget_credential(PROVIDER)
        .expect("credential removed");
    // Re-added with byte-identical material, which is the case a fingerprint
    // alone cannot distinguish.
    let live = live_assessment(&fixture.registry, &fixture.provider).expect("re-add");
    assert_eq!(
        fixture.registry.validate(
            fixture.session_id,
            &before,
            CapabilityBoundary::Dispatch,
            &live
        ),
        Err(CapabilityDenied)
    );
    let error = fixture
        .dispatch()
        .await
        .expect_err("a re-added credential is a new incarnation");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);
}

#[tokio::test]
async fn generation_exhaustion_changes_nothing_and_refuses_every_boundary() {
    let fixture = Fixture::staged(
        AssuranceProfile::Balanced,
        DeclaredCapabilityPolicy::default(),
    )
    .await;
    fixture.registry.pin_near_exhaustion_for_test();

    // The last advance is an ordinary revocation and takes effect.
    fixture.registry.revoke_all().expect("terminal advance");
    let terminal = fixture.registry.generation_counter();

    // Everything after it refuses and mutates nothing.
    assert_eq!(fixture.registry.revoke_all(), Err(CapabilityDenied));
    assert_eq!(
        fixture.registry.bump_policy_revision(),
        Err(CapabilityDenied)
    );
    assert_eq!(
        fixture.registry.set_profile(AssuranceProfile::Economy),
        Err(CapabilityDenied)
    );
    assert_eq!(fixture.registry.generation_counter(), terminal);
    assert_eq!(fixture.registry.profile(), AssuranceProfile::Balanced);

    let error = fixture
        .dispatch()
        .await
        .expect_err("an exhausted authority authorizes nothing");
    assert_capability_denial(&error);
    assert_eq!(fixture.backend.dispatches(), 0);

    let error = fixture
        .restage()
        .expect_err("an exhausted authority mints nothing either");
    assert_capability_denial(&error);
}

// ---------------------------------------------------------------------------
// Uniformity.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_denial_class_is_byte_identical_at_every_boundary() {
    let mut messages = BTreeSet::new();
    let mut codes: Vec<ComputerErrorCode> = Vec::new();

    // Stale at dispatch: the generation moved.
    {
        let fixture = Fixture::staged(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
        )
        .await;
        fixture.registry.revoke_all().expect("revoke");
        let error = fixture.dispatch().await.expect_err("stale");
        messages.insert(error.message.clone());
        codes.push(error.code);
    }

    // Stale at the next live frame. A separate run, because a refused
    // dispatch fails the run in flight and the kernel's own state machine —
    // not the capability authority — would answer the second call.
    {
        let fixture = Fixture::staged(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
        )
        .await;
        fixture.registry.revoke_all().expect("revoke");
        let error = fixture.observe().await.expect_err("stale frame");
        messages.insert(error.message.clone());
        codes.push(error.code);
    }

    // Drifted: the facts moved without a revocation.
    {
        let fixture = Fixture::staged(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
        )
        .await;
        fixture.provider.downgrade_to_observe();
        let error = fixture.dispatch().await.expect_err("drifted");
        messages.insert(error.message.clone());
        codes.push(error.code);
    }

    // Unknown: a reference this authority never issued.
    {
        let fixture = Fixture::ready(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
        )
        .await;
        let run = fixture.run();
        let error = fixture
            .service
            .bind_model_authority(
                &request_id(),
                &fixture.run_id,
                run.version,
                CapabilityBindingRef::unbound(),
            )
            .expect_err("unknown");
        messages.insert(error.message.clone());
        codes.push(error.code);
    }

    // Foreign: a binding issued by a different authority.
    {
        let fixture = Fixture::ready(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
        )
        .await;
        let other = registry(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
        );
        let foreign = qualify(
            &other,
            &fixture.provider,
            fixture.session_id,
            AssuranceProfile::Balanced,
        );
        let run = fixture.run();
        let error = fixture
            .service
            .bind_model_authority(&request_id(), &fixture.run_id, run.version, foreign)
            .expect_err("foreign");
        messages.insert(error.message.clone());
        codes.push(error.code);
    }

    assert_eq!(
        messages.len(),
        1,
        "stale, drifted, unknown and foreign must be one message: {messages:?}"
    );
    assert!(
        codes.windows(2).all(|pair| pair[0] == pair[1]),
        "and one code: {codes:?}"
    );
    assert_eq!(
        messages.iter().next().map(String::as_str),
        Some(CapabilityDenied::MESSAGE)
    );
}

#[tokio::test]
async fn every_profile_enforces_the_same_boundary() {
    for profile in AssuranceProfile::ALL {
        let fixture = Fixture::staged(profile, DeclaredCapabilityPolicy::default()).await;
        fixture.provider.downgrade_to_observe();
        let error = fixture.dispatch().await.unwrap_err_or_else_label(profile);
        assert_capability_denial(&error);
        assert_eq!(
            fixture.backend.dispatches(),
            0,
            "{profile:?} must refuse before the backend"
        );
    }
}

trait ExpectDenied {
    fn unwrap_err_or_else_label(self, profile: AssuranceProfile) -> ComputerError;
}

impl ExpectDenied for Result<ActionOutcome, ComputerError> {
    fn unwrap_err_or_else_label(self, profile: AssuranceProfile) -> ComputerError {
        self.err()
            .unwrap_or_else(|| panic!("{profile:?} allowed a downgraded capability to dispatch"))
    }
}
