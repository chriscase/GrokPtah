//! Adversarial coverage for the durable work-graph control plane.
//!
//! Every test here is a hostile case, not a happy path: two workers racing for
//! one item, a replayed lease, a graph reached from the wrong session, a cycle,
//! an exhausted budget, a cancel that collides with a settle, a restart in the
//! middle of a dispatch, a durable record that has been tampered with, a
//! provider send whose fate is unknown, a Computer Use grant taken over
//! mid-flight, mixed-provider receipts, reviewers that disagree, and evidence
//! that carries a secret.
//!
//! No provider is contacted. Every route is a loopback fixture with a synthetic
//! credential reference, and no test reads a credential store.

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ComputerRun, ComputerTarget, ComputerUseLimits, GrantIssuer,
    Sensitivity,
};
use grokptah_agent_bridge::orchestration::swarm::{
    self, ActionAuthority, AdmissionBlock, BoundOnlyRedactor, ClaimOutcome, DispatchProbe,
    EvidenceEntry, FailurePolicy, GrantBinding, GraphBudget, GraphId, GraphLifecycle,
    IsolationRequirement, LeaseId, LeaseState, PolicyRevisions, ProviderRouteSnapshot, QuorumGate,
    ReviewDecision, ReviewVerdict, SendCertainty, SwarmStore, WorkCapability, WorkGraphRecord,
    WorkGraphSpec, WorkId, WorkOutcome, WorkSpec, WorkState, WorkerBinding, WorkerId, WorkerRole,
    WorkerSpec, WORK_GRAPH_SCHEMA_VERSION,
};
use grokptah_agent_bridge::orchestration::{OrchStore, RunBounds};
use grokptah_agent_bridge::{
    CapabilitySource, ComputerUseTier, EffortLevel, ModelCapabilities, ProviderDeadlineClass,
    ProviderDialect, ProviderKind,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fixtures. Loopback only: no network, no credential store, no live provider.
// ---------------------------------------------------------------------------

fn work_id(value: &str) -> WorkId {
    WorkId::parse(value).expect("work id")
}

fn worker_id(value: &str) -> WorkerId {
    WorkerId::parse(value).expect("worker id")
}

fn bounds() -> RunBounds {
    RunBounds {
        max_prompt_bytes: 10_000,
        max_rounds: 4,
        max_duration_ms: 60_000,
    }
}

fn capabilities(items: &[WorkCapability]) -> BTreeSet<WorkCapability> {
    items.iter().copied().collect()
}

fn worker(id: &str, role: WorkerRole, caps: &[WorkCapability], provider: &str) -> WorkerSpec {
    WorkerSpec {
        worker_id: worker_id(id),
        role,
        binding: WorkerBinding {
            provider_id: provider.into(),
            profile_id: "loopback".into(),
            model_id: "synthetic-1".into(),
            effort: "medium".into(),
            computer_use_tier: if caps.contains(&WorkCapability::ComputerUse) {
                ComputerUseTier::SemanticAct
            } else {
                ComputerUseTier::None
            },
        },
        // Opaque keychain reference. Deliberately not a secret, and never
        // projected; the redaction test asserts it cannot reach a projection.
        credential_ref: "keychain://loopback/synthetic".into(),
        capabilities: capabilities(caps),
    }
}

fn item(id: &str, worker: &str, deps: &[&str], caps: &[WorkCapability]) -> WorkSpec {
    let caps_set = capabilities(caps);
    let isolation = if caps_set.iter().any(|c| c.requires_worktree()) {
        IsolationRequirement::DedicatedWorktree
    } else {
        IsolationRequirement::Shared
    };
    WorkSpec {
        work_id: work_id(id),
        worker_id: worker_id(worker),
        priority: 0,
        depends_on: deps.iter().map(|d| work_id(d)).collect(),
        capabilities: caps_set,
        isolation,
        quorum: None,
        bounds: bounds(),
        objective: format!("synthetic objective for {id}"),
    }
}

fn base_spec() -> WorkGraphSpec {
    WorkGraphSpec {
        schema_version: WORK_GRAPH_SCHEMA_VERSION,
        bounds_ceiling: bounds(),
        budget: GraphBudget::default(),
        workers: vec![worker(
            "w-read",
            WorkerRole::Investigate,
            &[WorkCapability::ReadWorkspace],
            "loopback",
        )],
        work: vec![
            item("a", "w-read", &[], &[WorkCapability::ReadWorkspace]),
            item("b", "w-read", &["a"], &[WorkCapability::ReadWorkspace]),
        ],
        failure_policy: FailurePolicy::BlockDependents,
    }
}

fn route(provider: &str, model: &str, effort: EffortLevel) -> ProviderRouteSnapshot {
    ProviderRouteSnapshot {
        schema_version: 1,
        provider_id: provider.into(),
        profile_id: "loopback".into(),
        model_id: model.into(),
        wire_model_id: model.into(),
        kind: ProviderKind::OpenAiCompatible,
        dialect: ProviderDialect::OpenAiChatCompletions,
        // Loopback only. No test in this file reaches a network.
        base_url: "http://127.0.0.1:1/v1".into(),
        endpoint_fingerprint: String::new(),
        credential_ref: "keychain://loopback/synthetic".into(),
        credential_fingerprint: "f".repeat(64),
        capabilities: ModelCapabilities {
            source: CapabilitySource::Measured,
            computer_use_tier: ComputerUseTier::SemanticAct,
            computer_capability_source: CapabilitySource::Measured,
            ..ModelCapabilities::default()
        },
        deadline_class: ProviderDeadlineClass::Standard,
        effort,
        snapshot_hash: String::new(),
    }
    .seal()
    .expect("route seals")
}

fn revisions() -> PolicyRevisions {
    PolicyRevisions {
        capability_revision: 7,
        policy_revision: 11,
    }
}

fn graph(spec: WorkGraphSpec, session_id: Uuid, workspace: &str) -> WorkGraphRecord {
    let mut record = WorkGraphRecord::new(
        GraphId::parse("graph-1").expect("graph id"),
        session_id,
        workspace,
        "agent-1",
        spec,
        Utc::now(),
    )
    .expect("graph record");
    swarm::recompute_derived(&mut record, Utc::now());
    record
}

fn authority_for(
    record: &WorkGraphRecord,
    intent: &swarm::DispatchIntent,
    route: ProviderRouteSnapshot,
) -> ActionAuthority {
    let authority_id =
        swarm::derive_authority_id(&intent.attempt_id, revisions()).expect("authority id");
    let now = Utc::now();
    ActionAuthority {
        schema_version: 1,
        authority_id,
        graph_id: record.graph_id.clone(),
        work_id: intent.work_id.clone(),
        worker_id: intent.worker_id.clone(),
        attempt_id: intent.attempt_id.clone(),
        attempt: intent.attempt,
        session_id: record.session_id,
        workspace: record.workspace.clone(),
        agent_id: record.agent_id.clone(),
        route,
        revisions: revisions(),
        capabilities: intent.capabilities.clone(),
        bounds: bounds(),
        issued_at: now,
        expires_at: now + Duration::minutes(5),
        binding_hash: String::new(),
    }
    .seal()
    .expect("authority seals")
}

fn lease_id_for(intent: &swarm::DispatchIntent) -> LeaseId {
    LeaseId::parse(format!("lease-{}", intent.attempt_id)).expect("lease id")
}

/// Issue the first admitted intent, returning the intent and its lease id.
fn issue_first(record: &mut WorkGraphRecord, slots: usize) -> (swarm::DispatchIntent, LeaseId) {
    let plan = swarm::plan_admissions(record, slots, Utc::now());
    let intent = plan.intents.first().cloned().expect("an admitted intent");
    let lease_id = lease_id_for(&intent);
    let authority = authority_for(
        record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    swarm::issue_lease(
        record,
        &intent,
        lease_id.clone(),
        &authority,
        None,
        Utc::now(),
    )
    .expect("lease issues");
    (intent, lease_id)
}

fn open_store(dir: &std::path::Path) -> SwarmStore {
    SwarmStore::new(OrchStore::open(dir).expect("orch store opens"))
}

// ---------------------------------------------------------------------------
// Dependency ordering, cycles, and deterministic assignment
// ---------------------------------------------------------------------------

#[test]
fn dependency_cycles_are_rejected_deterministically() {
    let mut spec = base_spec();
    spec.work = vec![
        item("a", "w-read", &["c"], &[WorkCapability::ReadWorkspace]),
        item("b", "w-read", &["a"], &[WorkCapability::ReadWorkspace]),
        item("c", "w-read", &["b"], &[WorkCapability::ReadWorkspace]),
    ];
    let first = spec.validate().expect_err("a cycle must be rejected");
    assert!(
        first.message.contains("dependency cycle"),
        "unexpected message: {}",
        first.message
    );
    // Determinism: the same graph reports the same members, in the same order.
    for _ in 0..8 {
        let again = spec.validate().expect_err("still rejected");
        assert_eq!(again.message, first.message);
    }
    assert!(first.message.contains("a, b, c"), "{}", first.message);
}

#[test]
fn self_dependency_and_unknown_dependency_are_rejected() {
    let mut spec = base_spec();
    spec.work[1].depends_on = vec![work_id("b")];
    assert!(spec.validate().is_err(), "self dependency must be rejected");

    let mut spec = base_spec();
    spec.work[1].depends_on = vec![work_id("nope")];
    assert!(
        spec.validate().is_err(),
        "unknown dependency must be rejected"
    );
}

#[test]
fn assignment_order_is_deterministic_by_priority_then_id() {
    let mut spec = base_spec();
    spec.budget.max_in_flight = 8;
    spec.work = vec![
        item("zebra", "w-read", &[], &[WorkCapability::ReadWorkspace]),
        item("alpha", "w-read", &[], &[WorkCapability::ReadWorkspace]),
        item("middle", "w-read", &[], &[WorkCapability::ReadWorkspace]),
    ];
    spec.work[2].priority = 5;
    let record = graph(spec, Uuid::new_v4(), "/tmp/ws");
    let mut seen = Vec::new();
    for _ in 0..8 {
        let plan = swarm::plan_admissions(&record, 8, Utc::now());
        let order: Vec<String> = plan
            .intents
            .iter()
            .map(|intent| intent.work_id.to_string())
            .collect();
        seen.push(order);
    }
    // Highest priority first, then work id ascending. Never hash order.
    assert_eq!(seen[0], vec!["middle", "alpha", "zebra"]);
    assert!(seen.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn a_dependent_is_not_ready_until_its_dependency_succeeds() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    assert_eq!(plan.intents.len(), 1, "only the root is ready");
    assert_eq!(plan.intents[0].work_id.to_string(), "a");

    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    swarm::settle(&mut record, &lease, &WorkOutcome::succeeded(), Utc::now()).expect("settle");

    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    assert_eq!(plan.intents.len(), 1);
    assert_eq!(plan.intents[0].work_id.to_string(), "b");
}

#[test]
fn a_failure_blocks_dependents_and_spares_independent_branches() {
    let mut spec = base_spec();
    spec.work.push(item(
        "independent",
        "w-read",
        &[],
        &[WorkCapability::ReadWorkspace],
    ));
    let mut record = graph(spec, Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    swarm::settle(
        &mut record,
        &lease,
        &WorkOutcome::failed("synthetic failure"),
        Utc::now(),
    )
    .expect("settle");

    assert_eq!(
        record.work_record(&work_id("b")).map(|r| r.state),
        Some(WorkState::Blocked),
        "dependent must be blocked"
    );
    assert_eq!(
        record.work_record(&work_id("independent")).map(|r| r.state),
        Some(WorkState::Ready),
        "independent branch must be spared"
    );
}

// ---------------------------------------------------------------------------
// Concurrency: duplicate assignment, concurrent workers, stale/replayed leases
// ---------------------------------------------------------------------------

#[test]
fn two_workers_racing_one_item_produce_exactly_one_spawn_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);

    let first = store
        .claim_lease_spawn(&lease, "worker-a")
        .expect("claim a");
    let second = store
        .claim_lease_spawn(&lease, "worker-b")
        .expect("claim b");
    assert_eq!(first, ClaimOutcome::Won);
    assert_eq!(
        second,
        ClaimOutcome::AlreadyHeld,
        "a lease has exactly one spawn winner"
    );
    assert_eq!(
        store.lease_spawn_holder(&lease).expect("holder"),
        Some("worker-a".to_string()),
        "the loser must not overwrite the winner"
    );
}

#[test]
fn replaying_a_dispatch_intent_returns_the_stored_lease_and_never_a_second_one() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let lease_id = lease_id_for(&intent);
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );

    let first = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id.clone(),
        &authority,
        None,
        Utc::now(),
    )
    .expect("first issue");
    let replay = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id.clone(),
        &authority,
        None,
        Utc::now(),
    )
    .expect("replay returns the stored lease");

    assert_eq!(first.lease_id, replay.lease_id);
    assert_eq!(
        record.leases.len(),
        1,
        "replay must not mint a second lease"
    );
    assert_eq!(
        record.budget.attempts_used, 1,
        "replay must not move the attempt counter"
    );
}

#[test]
fn a_second_lease_id_for_the_same_attempt_is_a_conflict() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    swarm::issue_lease(
        &mut record,
        &intent,
        lease_id_for(&intent),
        &authority,
        None,
        Utc::now(),
    )
    .expect("first issue");

    let hostile = LeaseId::parse("lease-duplicate").expect("lease id");
    let error = swarm::issue_lease(&mut record, &intent, hostile, &authority, None, Utc::now())
        .expect_err("a duplicate assignment must be refused");
    assert!(
        error.message.contains("already has a durable lease"),
        "{error}"
    );
}

#[test]
fn an_intent_planned_under_a_superseded_epoch_is_stale() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );

    // A control action bumps the graph epoch between planning and issuing.
    record.epoch = record.epoch.saturating_add(1);

    let error = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id_for(&intent),
        &authority,
        None,
        Utc::now(),
    )
    .expect_err("a stale intent must be refused");
    assert!(error.message.contains("superseded"), "{error}");
}

#[test]
fn acknowledging_a_lease_from_an_earlier_epoch_is_refused() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    record.epoch = record.epoch.saturating_add(1);
    let error = swarm::acknowledge(&mut record, &lease, "child-1", Utc::now())
        .expect_err("a stale lease must not acknowledge");
    assert!(error.message.contains("superseded"), "{error}");
}

#[test]
fn compare_and_swap_refuses_a_writer_whose_revision_moved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    store.create_graph(&record).expect("create");

    // Two readers both observe revision 1.
    let reader_a = store
        .load_graph(&record.graph_id)
        .expect("load")
        .expect("some");
    let reader_b = store
        .load_graph(&record.graph_id)
        .expect("load")
        .expect("some");
    assert_eq!(reader_a.revision, reader_b.revision);

    store
        .compare_and_swap(reader_a.revision, &reader_a, Utc::now())
        .expect("first writer commits");
    let error = store
        .compare_and_swap(reader_b.revision, &reader_b, Utc::now())
        .expect_err("second writer must lose");
    assert!(error.message.contains("revision moved"), "{error}");
}

#[test]
fn a_graph_id_cannot_be_created_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    store.create_graph(&record).expect("create");
    assert!(
        store.create_graph(&record).is_err(),
        "a second create for the same id must be refused"
    );
}

// ---------------------------------------------------------------------------
// Cross-workspace and cross-session denial
// ---------------------------------------------------------------------------

#[test]
fn authority_is_refused_for_another_session_workspace_agent_or_route() {
    let session = Uuid::new_v4();
    let mut record = graph(base_spec(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let route = route("loopback", "synthetic-1", EffortLevel::Medium);
    let authority = authority_for(&record, &intent, route.clone());
    let now = Utc::now();

    let base = swarm::AuthorityUse {
        graph_id: &record.graph_id,
        work_id: &intent.work_id,
        attempt_id: &intent.attempt_id,
        attempt: intent.attempt,
        session_id: session,
        workspace: &record.workspace,
        agent_id: &record.agent_id,
        route_snapshot_hash: &route.snapshot_hash,
        revisions: revisions(),
        capability: WorkCapability::ReadWorkspace,
    };
    authority
        .verify(&base, now)
        .expect("the exact claim verifies");

    let other_session = Uuid::new_v4();
    let mut claim = base.clone();
    claim.session_id = other_session;
    assert!(
        authority.verify(&claim, now).is_err(),
        "another session must be refused"
    );

    let mut claim = base.clone();
    claim.workspace = "/tmp/other";
    assert!(
        authority.verify(&claim, now).is_err(),
        "another workspace must be refused"
    );

    let mut claim = base.clone();
    claim.agent_id = "agent-2";
    assert!(
        authority.verify(&claim, now).is_err(),
        "another agent must be refused"
    );

    let other_route = self::route("other-provider", "synthetic-1", EffortLevel::Medium);
    let mut claim = base.clone();
    claim.route_snapshot_hash = &other_route.snapshot_hash;
    assert!(
        authority.verify(&claim, now).is_err(),
        "another provider route must be refused"
    );

    let mut claim = base.clone();
    claim.capability = WorkCapability::WriteWorkspace;
    assert!(
        authority.verify(&claim, now).is_err(),
        "a capability outside the authority must be refused"
    );

    // Same provider and model, different effort: still a different route.
    let effort_route = self::route("loopback", "synthetic-1", EffortLevel::High);
    let mut claim = base.clone();
    claim.route_snapshot_hash = &effort_route.snapshot_hash;
    assert!(
        authority.verify(&claim, now).is_err(),
        "a different effort must be refused"
    );

    // Keep the record referenced so the borrow above stays honest.
    swarm::recompute_derived(&mut record, now);
}

#[test]
fn authority_minted_under_older_revisions_is_stale() {
    let session = Uuid::new_v4();
    let record = graph(base_spec(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let route = route("loopback", "synthetic-1", EffortLevel::Medium);
    let authority = authority_for(&record, &intent, route.clone());

    let bumped = PolicyRevisions {
        capability_revision: revisions().capability_revision + 1,
        policy_revision: revisions().policy_revision,
    };
    let claim = swarm::AuthorityUse {
        graph_id: &record.graph_id,
        work_id: &intent.work_id,
        attempt_id: &intent.attempt_id,
        attempt: intent.attempt,
        session_id: session,
        workspace: &record.workspace,
        agent_id: &record.agent_id,
        route_snapshot_hash: &route.snapshot_hash,
        revisions: bumped,
        capability: WorkCapability::ReadWorkspace,
    };
    let error = authority
        .verify(&claim, Utc::now())
        .expect_err("a revision bump must invalidate the authority");
    assert!(error.message.contains("revisions"), "{error}");
}

#[test]
fn an_authority_bound_to_another_attempt_is_refused() {
    let session = Uuid::new_v4();
    let record = graph(base_spec(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let route = route("loopback", "synthetic-1", EffortLevel::Medium);
    let authority = authority_for(&record, &intent, route.clone());

    let other_attempt =
        swarm::derive_attempt_id(&record.graph_id, &intent.work_id, intent.attempt + 1)
            .expect("attempt id");
    let claim = swarm::AuthorityUse {
        graph_id: &record.graph_id,
        work_id: &intent.work_id,
        attempt_id: &other_attempt,
        attempt: intent.attempt + 1,
        session_id: session,
        workspace: &record.workspace,
        agent_id: &record.agent_id,
        route_snapshot_hash: &route.snapshot_hash,
        revisions: revisions(),
        capability: WorkCapability::ReadWorkspace,
    };
    assert!(
        authority.verify(&claim, Utc::now()).is_err(),
        "an authority is bound to exactly one attempt"
    );
}

#[test]
fn a_single_use_authority_cannot_be_consumed_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );

    assert_eq!(
        store
            .consume_authority(&authority.authority_id, "holder-a")
            .expect("consume"),
        ClaimOutcome::Won
    );
    assert_eq!(
        store
            .consume_authority(&authority.authority_id, "holder-b")
            .expect("consume"),
        ClaimOutcome::AlreadyHeld,
        "authority is single use"
    );
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

#[test]
fn attempt_budget_exhaustion_stops_admission_with_a_named_reason() {
    let mut spec = base_spec();
    spec.budget = GraphBudget {
        max_total_attempts: 1,
        max_in_flight: 4,
        ..GraphBudget::default()
    };
    let mut record = graph(spec, Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    swarm::settle(&mut record, &lease, &WorkOutcome::succeeded(), Utc::now()).expect("settle");

    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    assert!(plan.intents.is_empty());
    assert_eq!(plan.blocked_by, AdmissionBlock::AttemptBudgetExhausted);
    assert_eq!(
        plan.ready_not_admitted, 0,
        "a hard budget refusal reports before counting candidates"
    );
}

#[test]
fn token_budget_exhaustion_stops_admission_but_never_cancels_live_work() {
    let mut spec = base_spec();
    spec.budget.max_total_tokens = 10;
    let mut record = graph(spec, Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    let attempt = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");
    swarm::record_attempt_finished(
        &mut record,
        &attempt.attempt_id,
        SendCertainty::KnownAccepted,
        Some(200),
        Some(grokptah_agent_bridge::CompletionUsage {
            prompt_tokens: 40,
            completion_tokens: 20,
            total_tokens: 60,
            requests: 1,
        }),
        Utc::now(),
    )
    .expect("finish");

    assert_eq!(record.budget.tokens_used, 60);
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    assert_eq!(plan.blocked_by, AdmissionBlock::TokenBudgetExhausted);
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::Running),
        "an exhausted budget must not cancel work that is already running"
    );
}

#[test]
fn the_graph_in_flight_cap_narrows_host_capacity_and_never_widens_it() {
    let mut spec = base_spec();
    spec.budget.max_in_flight = 1;
    spec.work = vec![
        item("a", "w-read", &[], &[WorkCapability::ReadWorkspace]),
        item("b", "w-read", &[], &[WorkCapability::ReadWorkspace]),
        item("c", "w-read", &[], &[WorkCapability::ReadWorkspace]),
    ];
    let record = graph(spec, Uuid::new_v4(), "/tmp/ws");

    // The host offers 8 slots; the graph still admits only its own cap.
    let plan = swarm::plan_admissions(&record, 8, Utc::now());
    assert_eq!(plan.intents.len(), 1);
    assert_eq!(plan.ready_not_admitted, 2);

    // The host offers none; the graph admits none and says why.
    let plan = swarm::plan_admissions(&record, 0, Utc::now());
    assert!(plan.intents.is_empty());
    assert_eq!(plan.blocked_by, AdmissionBlock::NoSlots);
}

#[test]
fn a_graph_past_its_deadline_admits_nothing() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    record.deadline_at = Utc::now() - Duration::seconds(1);
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    assert_eq!(plan.blocked_by, AdmissionBlock::DeadlineExceeded);
}

// ---------------------------------------------------------------------------
// Cancellation races, timeouts, and truthful terminal state
// ---------------------------------------------------------------------------

#[test]
fn cancelling_live_work_waits_for_confirmation_and_never_invents_a_result() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");

    let state = swarm::cancel_work(&mut record, &work_id("a"), Utc::now()).expect("cancel");
    assert_eq!(
        state,
        WorkState::Cancelling,
        "a live child is not settled by the cancel itself"
    );
    assert!(!state.is_settled());

    // The owner's confirmation is what settles it.
    swarm::settle(&mut record, &lease, &WorkOutcome::cancelled(), Utc::now()).expect("settle");
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::Cancelled)
    );
}

#[test]
fn cancelling_work_that_never_started_settles_immediately_and_revokes_its_lease() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let state = swarm::cancel_work(&mut record, &work_id("a"), Utc::now()).expect("cancel");
    assert_eq!(state, WorkState::Cancelled);
    assert_eq!(
        record.work_record(&work_id("b")).map(|r| r.state),
        Some(WorkState::Blocked),
        "a cancelled dependency blocks its dependents"
    );
}

#[test]
fn a_cancel_racing_a_settle_does_not_lose_the_owners_terminal_report() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");

    // The owner's success lands first; the cancel arrives afterwards.
    swarm::settle(&mut record, &lease, &WorkOutcome::succeeded(), Utc::now()).expect("settle");
    let state = swarm::cancel_work(&mut record, &work_id("a"), Utc::now()).expect("cancel");
    assert_eq!(
        state,
        WorkState::Succeeded,
        "a settled item keeps its truthful outcome"
    );

    // And a second settle for the same lease is refused rather than silently
    // rewriting history.
    let error = swarm::settle(&mut record, &lease, &WorkOutcome::cancelled(), Utc::now())
        .expect_err("double settle must be refused");
    assert!(error.message.contains("already settled"), "{error}");
}

#[test]
fn a_whole_graph_cancel_will_not_report_cancelled_while_a_child_may_be_running() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");

    // Restart makes the unacknowledged lease uncertain.
    swarm::recover(&mut record, Utc::now());
    swarm::cancel_graph(&mut record, "operator stopped the graph", Utc::now()).expect("cancel");
    let lifecycle = swarm::settle_lifecycle(&mut record, Utc::now());
    assert_eq!(
        lifecycle,
        GraphLifecycle::Cancelling,
        "an outstanding uncertainty withholds the terminal state"
    );
    assert!(record.has_uncertainty());

    // Positive evidence that it never started resolves the uncertainty.
    swarm::reconcile_uncertain(&mut record, &lease, &DispatchProbe::NotStarted, Utc::now())
        .expect("reconcile");
    swarm::cancel_work(&mut record, &work_id("a"), Utc::now()).expect("cancel a");
    swarm::cancel_work(&mut record, &work_id("b"), Utc::now()).expect("cancel b");
    let lifecycle = swarm::settle_lifecycle(&mut record, Utc::now());
    assert_eq!(lifecycle, GraphLifecycle::Cancelled);
}

#[test]
fn an_expired_execution_bound_times_out_unstarted_work_and_makes_live_work_uncertain() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");

    let past = Utc::now() + Duration::minutes(10);
    assert_eq!(swarm::sweep_timeouts(&mut record, past), 1);
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::TimedOut),
        "a lease that never acknowledged times out truthfully"
    );

    // A child that did acknowledge may still be running when its bound passes.
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    assert_eq!(swarm::sweep_timeouts(&mut record, past), 1);
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::DispatchUncertain),
        "a live child's fate is unknown, not failed"
    );
}

// ---------------------------------------------------------------------------
// Restart, replay, and uncertain dispatch
// ---------------------------------------------------------------------------

#[test]
fn restart_marks_unacknowledged_leases_uncertain_and_leaves_acknowledged_ones_running() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");

    let report = swarm::recover(&mut record, Utc::now());
    assert_eq!(report.leases_marked_uncertain, 1);
    assert_eq!(
        record.lease(&lease).map(|l| l.state),
        Some(LeaseState::Uncertain)
    );
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::DispatchUncertain)
    );

    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    let report = swarm::recover(&mut record, Utc::now());
    assert_eq!(report.leases_marked_uncertain, 0);
    assert_eq!(
        record.lease(&lease).map(|l| l.state),
        Some(LeaseState::Acknowledged),
        "an acknowledged lease carries a handle and is left running"
    );
}

#[test]
fn an_uncertain_dispatch_is_never_re_admitted_without_positive_evidence() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::recover(&mut record, Utc::now());

    let plan = swarm::plan_admissions(&record, 8, Utc::now());
    assert!(
        plan.intents.iter().all(|i| i.work_id.to_string() != "a"),
        "an uncertain item must not be re-admitted"
    );

    // Unknown resolves nothing, on purpose.
    let moved =
        swarm::reconcile_uncertain(&mut record, &lease, &DispatchProbe::Unknown, Utc::now())
            .expect("probe");
    assert!(!moved, "Unknown must resolve nothing");
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::DispatchUncertain)
    );

    // Proof that it is running resolves it forward, not backward.
    swarm::reconcile_uncertain(
        &mut record,
        &lease,
        &DispatchProbe::Running {
            external_ref: "child-1".into(),
        },
        Utc::now(),
    )
    .expect("probe");
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::Running)
    );
}

#[test]
fn an_uncertain_provider_send_forbids_a_same_work_retry() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    let attempt = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");

    swarm::record_attempt_finished(
        &mut record,
        &attempt.attempt_id,
        SendCertainty::UncertainAccept,
        None,
        None,
        Utc::now(),
    )
    .expect("finish");

    assert!(swarm::forbids_same_work_retry(&record, &work_id("a")));
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.state),
        Some(WorkState::DispatchUncertain),
        "an uncertain send makes the item uncertain, not failed"
    );
    let plan = swarm::plan_admissions(&record, 8, Utc::now());
    assert!(plan.intents.iter().all(|i| i.work_id.to_string() != "a"));
}

#[test]
fn a_known_not_sent_attempt_is_safe_to_retry_in_the_same_work_item() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    let attempt = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");
    swarm::record_attempt_finished(
        &mut record,
        &attempt.attempt_id,
        SendCertainty::KnownNotSent,
        None,
        None,
        Utc::now(),
    )
    .expect("finish");
    assert!(!swarm::forbids_same_work_retry(&record, &work_id("a")));
}

#[test]
fn a_known_not_sent_attempt_cannot_claim_an_http_status_or_usage() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    let attempt = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");

    assert!(
        swarm::record_attempt_finished(
            &mut record,
            &attempt.attempt_id,
            SendCertainty::KnownNotSent,
            Some(500),
            None,
            Utc::now(),
        )
        .is_err(),
        "a send that never happened cannot have an HTTP status"
    );
    assert!(
        swarm::record_attempt_finished(
            &mut record,
            &attempt.attempt_id,
            SendCertainty::UncertainAccept,
            None,
            Some(grokptah_agent_bridge::CompletionUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                requests: 1,
            }),
            Utc::now(),
        )
        .is_err(),
        "usage requires a known accepted response"
    );
}

#[test]
fn restart_treats_an_admitted_attempt_row_as_a_possible_accept() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child-1", Utc::now()).expect("ack");
    let attempt = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");

    let report = swarm::recover(&mut record, Utc::now());
    assert_eq!(report.attempts_marked_uncertain, 1);
    assert_eq!(
        record
            .attempt(&attempt.attempt_id)
            .and_then(|a| a.send_certainty),
        Some(SendCertainty::UncertainAccept)
    );
}

#[test]
fn a_replayed_attempt_admission_reuses_its_row_and_never_moves_the_ordinal() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    let first = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");
    let replay = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("replay");
    assert_eq!(first.ordinal, replay.ordinal);
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(
        record.work_record(&work_id("a")).map(|r| r.send_ordinal),
        Some(1)
    );
}

#[test]
fn attempt_identity_is_derivable_and_stable_across_replay() {
    let graph_id = GraphId::parse("graph-1").expect("graph id");
    let first = swarm::derive_attempt_id(&graph_id, &work_id("a"), 1).expect("id");
    let again = swarm::derive_attempt_id(&graph_id, &work_id("a"), 1).expect("id");
    let next = swarm::derive_attempt_id(&graph_id, &work_id("a"), 2).expect("id");
    let other_work = swarm::derive_attempt_id(&graph_id, &work_id("b"), 1).expect("id");
    assert_eq!(first, again, "replay proposes the identity already on disk");
    assert_ne!(first, next);
    assert_ne!(first, other_work);
}

#[test]
fn a_durable_graph_round_trips_and_replays_its_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session = Uuid::new_v4();
    let graph_id;
    let expected_state;
    {
        let store = open_store(dir.path());
        let mut record = graph(base_spec(), session, "/tmp/ws");
        graph_id = record.graph_id.clone();
        let (_, lease) = issue_first(&mut record, 4);
        swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
        store.create_graph(&record).expect("create");
        expected_state = record
            .work_record(&work_id("a"))
            .map(|r| r.state)
            .expect("state");
    }
    // Reopening the ledger is what a restart looks like from here.
    let store = open_store(dir.path());
    let mut reloaded = store.load_graph(&graph_id).expect("load").expect("some");
    assert_eq!(
        reloaded.work_record(&work_id("a")).map(|r| r.state),
        Some(expected_state)
    );
    let report = swarm::recover(&mut reloaded, Utc::now());
    assert_eq!(report.leases_marked_uncertain, 1);
}

// ---------------------------------------------------------------------------
// Malformed durable records
// ---------------------------------------------------------------------------

#[test]
fn a_tampered_durable_record_fails_closed_rather_than_being_partially_honored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    store.create_graph(&record).expect("create");

    let path = dir
        .path()
        .join("swarm/graphs")
        .read_dir()
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .expect("graph file");

    let original = std::fs::read_to_string(&path).expect("read");

    // 1. A work record for undeclared work.
    let mut tampered: serde_json::Value = serde_json::from_str(&original).expect("json");
    tampered["work"][0]["workId"] = serde_json::json!("smuggled");
    std::fs::write(&path, tampered.to_string()).expect("write");
    assert!(
        store.load_graph(&record.graph_id).is_err(),
        "a record naming undeclared work must fail closed"
    );

    // 2. A path-traversing identity that serde would otherwise accept.
    let mut tampered: serde_json::Value = serde_json::from_str(&original).expect("json");
    tampered["work"][0]["workId"] = serde_json::json!("../../escape");
    std::fs::write(&path, tampered.to_string()).expect("write");
    assert!(
        store.load_graph(&record.graph_id).is_err(),
        "a path-traversing identity must fail closed"
    );

    // 3. An unsupported schema version.
    let mut tampered: serde_json::Value = serde_json::from_str(&original).expect("json");
    tampered["schemaVersion"] = serde_json::json!(999);
    std::fs::write(&path, tampered.to_string()).expect("write");
    assert!(store.load_graph(&record.graph_id).is_err());

    // 4. Outright corrupt bytes.
    std::fs::write(&path, "{not json").expect("write");
    assert!(store.load_graph(&record.graph_id).is_err());

    // 5. A record that claims another graph's identity.
    let mut tampered: serde_json::Value = serde_json::from_str(&original).expect("json");
    tampered["graphId"] = serde_json::json!("graph-2");
    std::fs::write(&path, tampered.to_string()).expect("write");
    assert!(store.load_graph(&record.graph_id).is_err());

    // A broken ledger is reported, not hidden.
    let (records, skipped) = store.list_graphs().expect("list");
    assert!(records.is_empty());
    assert_eq!(skipped, 1, "unreadable records must be counted");

    std::fs::write(&path, original).expect("restore");
    assert!(store.load_graph(&record.graph_id).expect("load").is_some());
}

#[test]
fn an_unknown_specification_field_fails_closed() {
    let json = serde_json::json!({
        "schemaVersion": WORK_GRAPH_SCHEMA_VERSION,
        "boundsCeiling": bounds(),
        "budget": GraphBudget::default(),
        "workers": [],
        "work": [],
        "failurePolicy": "block_dependents",
        "smuggledField": true,
    });
    assert!(
        serde_json::from_value::<WorkGraphSpec>(json).is_err(),
        "an unrecognized field must fail closed, not be silently dropped"
    );
}

#[test]
fn browser_and_raw_host_authority_are_not_expressible() {
    for hostile in ["browser", "raw_host", "computer_use_visual", "network"] {
        let json = serde_json::json!(hostile);
        let parsed = serde_json::from_value::<WorkCapability>(json);
        if hostile == "computer_use_visual" || hostile == "browser" || hostile == "raw_host" {
            assert!(
                parsed.is_err(),
                "{hostile} must not be expressible as a capability"
            );
        } else {
            assert!(parsed.is_err(), "{hostile} must not be a capability");
        }
    }
    // The closed set is exactly these four.
    for allowed in [
        "read_workspace",
        "write_workspace",
        "execute_in_worktree",
        "computer_use",
    ] {
        assert!(serde_json::from_value::<WorkCapability>(serde_json::json!(allowed)).is_ok());
    }
}

// ---------------------------------------------------------------------------
// Computer Use grant binding, consumption, and takeover
// ---------------------------------------------------------------------------

fn computer_fixture(session: Uuid) -> (ComputerRun, ActionGrant) {
    let target = ComputerTarget {
        app_id: "com.example.synthetic".into(),
        window_id: "window-1".into(),
        generation: 1,
        display_name: "Synthetic".into(),
        sensitivity: Sensitivity::None,
    };
    let mut run = ComputerRun::new(
        session,
        Some("/tmp/ws".into()),
        target.clone(),
        ComputerUseLimits::default(),
    )
    .expect("computer run");
    let now = Utc::now();
    let grant = ActionGrant {
        grant_id: "grant-1".into(),
        run_id: run.run_id.clone(),
        target,
        action_classes: [ActionClass::Semantic].into_iter().collect(),
        issued_by: GrantIssuer::LocalUser,
        issued_at: now - Duration::seconds(1),
        expires_at: now + Duration::minutes(5),
        uses_remaining: Some(4),
        revoked_at: None,
    };
    run.grant = Some(grant.clone());
    (run, grant)
}

fn computer_graph() -> WorkGraphSpec {
    let mut spec = base_spec();
    spec.workers.push(worker(
        "w-cu",
        WorkerRole::Investigate,
        &[WorkCapability::ReadWorkspace, WorkCapability::ComputerUse],
        "loopback",
    ));
    spec.work = vec![item(
        "cu",
        "w-cu",
        &[],
        &[WorkCapability::ReadWorkspace, WorkCapability::ComputerUse],
    )];
    spec
}

#[test]
fn computer_use_work_cannot_dispatch_without_a_bound_grant() {
    let session = Uuid::new_v4();
    let mut record = graph(computer_graph(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    assert!(intent.requires_computer_use);
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    let error = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id_for(&intent),
        &authority,
        None,
        Utc::now(),
    )
    .expect_err("a Computer Use item needs a grant");
    assert!(error.message.contains("Computer Use"), "{error}");
}

#[test]
fn a_grant_attached_to_work_that_does_not_require_it_is_refused() {
    let session = Uuid::new_v4();
    let (run, grant) = computer_fixture(session);
    let mut record = graph(base_spec(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let lease_id = lease_id_for(&intent);
    let binding = swarm::bind_grant(&grant, &run, &lease_id, &intent.attempt_id).expect("bind");
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    assert!(
        swarm::issue_lease(
            &mut record,
            &intent,
            lease_id,
            &authority,
            Some(binding),
            Utc::now()
        )
        .is_err(),
        "an unnecessary grant must be refused"
    );
}

#[test]
fn a_grant_binding_cannot_be_moved_to_another_lease() {
    let session = Uuid::new_v4();
    let (run, grant) = computer_fixture(session);
    let record = graph(computer_graph(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let lease_id = lease_id_for(&intent);
    let binding = swarm::bind_grant(&grant, &run, &lease_id, &intent.attempt_id).expect("bind");

    binding
        .verify_binding(&lease_id, &intent.attempt_id)
        .expect("the exact binding verifies");
    let other_lease = LeaseId::parse("lease-elsewhere").expect("lease id");
    assert!(
        binding
            .verify_binding(&other_lease, &intent.attempt_id)
            .is_err(),
        "a binding cannot be copied to another lease"
    );
    let other_attempt =
        swarm::derive_attempt_id(&record.graph_id, &intent.work_id, 9).expect("attempt id");
    assert!(
        binding.verify_binding(&lease_id, &other_attempt).is_err(),
        "a binding cannot be copied to another attempt"
    );
}

#[test]
fn a_computer_use_takeover_makes_every_earlier_binding_unusable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let session = Uuid::new_v4();
    let (mut run, grant) = computer_fixture(session);
    let mut record = graph(computer_graph(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let lease_id = lease_id_for(&intent);
    let binding = swarm::bind_grant(&grant, &run, &lease_id, &intent.attempt_id).expect("bind");
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    let lease = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id.clone(),
        &authority,
        Some(binding),
        Utc::now(),
    )
    .expect("lease issues");

    // The bound holder consumes successfully.
    swarm::consume_grant_for_action(
        &store,
        &lease,
        &grant,
        &run,
        ActionClass::Semantic,
        session,
        Utc::now(),
    )
    .expect("the bound holder consumes");

    // Now a takeover advances the run's control epoch.
    run.control_epoch = swarm::revoke_bound_grants(&run);
    let error = swarm::consume_grant_for_action(
        &store,
        &lease,
        &grant,
        &run,
        ActionClass::Semantic,
        session,
        Utc::now(),
    )
    .expect_err("a superseded binding must not consume");
    assert!(error.message.contains("control epoch"), "{error}");
}

#[test]
fn a_second_holder_cannot_consume_a_grant_already_consumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let session = Uuid::new_v4();
    let (run, grant) = computer_fixture(session);
    let mut record = graph(computer_graph(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let lease_id = lease_id_for(&intent);
    let binding = swarm::bind_grant(&grant, &run, &lease_id, &intent.attempt_id).expect("bind");
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    let lease = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id.clone(),
        &authority,
        Some(binding.clone()),
        Utc::now(),
    )
    .expect("lease issues");

    swarm::consume_grant_for_action(
        &store,
        &lease,
        &grant,
        &run,
        ActionClass::Semantic,
        session,
        Utc::now(),
    )
    .expect("first consumption wins");

    // The same lease and attempt replaying its own consumption is idempotent.
    swarm::consume_grant_for_action(
        &store,
        &lease,
        &grant,
        &run,
        ActionClass::Semantic,
        session,
        Utc::now(),
    )
    .expect("the same holder may replay");

    // A different lease presenting the same grant loses.
    let mut hostile = lease.clone();
    hostile.lease_id = LeaseId::parse("lease-hostile").expect("lease id");
    let mut hostile_binding = binding;
    hostile_binding.binding_hash = GrantBinding::new(
        swarm::GrantId::parse(grant.grant_id.clone()).expect("grant id"),
        run.run_id.clone(),
        swarm::grant::target_fingerprint(&run),
        run.owner_session_id,
        run.control_epoch,
        &hostile.lease_id,
        &hostile.attempt_id,
    )
    .expect("rebind")
    .binding_hash;
    hostile.grant = Some(hostile_binding);
    let error = swarm::consume_grant_for_action(
        &store,
        &hostile,
        &grant,
        &run,
        ActionClass::Semantic,
        session,
        Utc::now(),
    )
    .expect_err("a second holder must lose");
    assert!(error.message.contains("already consumed"), "{error}");
}

#[test]
fn a_revoked_expired_or_out_of_class_grant_never_consumes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let session = Uuid::new_v4();
    let (run, grant) = computer_fixture(session);
    let mut record = graph(computer_graph(), session, "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let lease_id = lease_id_for(&intent);
    let binding = swarm::bind_grant(&grant, &run, &lease_id, &intent.attempt_id).expect("bind");
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    let lease = swarm::issue_lease(
        &mut record,
        &intent,
        lease_id,
        &authority,
        Some(binding),
        Utc::now(),
    )
    .expect("lease issues");

    let mut revoked = grant.clone();
    revoked.revoked_at = Some(Utc::now());
    assert!(
        swarm::consume_grant_for_action(
            &store,
            &lease,
            &revoked,
            &run,
            ActionClass::Semantic,
            session,
            Utc::now()
        )
        .is_err(),
        "a revoked grant never consumes"
    );

    let mut expired = grant.clone();
    expired.expires_at = Utc::now() - Duration::seconds(1);
    assert!(
        swarm::consume_grant_for_action(
            &store,
            &lease,
            &expired,
            &run,
            ActionClass::Semantic,
            session,
            Utc::now()
        )
        .is_err(),
        "an expired grant never consumes"
    );

    assert!(
        swarm::consume_grant_for_action(
            &store,
            &lease,
            &grant,
            &run,
            ActionClass::TextEntry,
            session,
            Utc::now()
        )
        .is_err(),
        "an action class outside the grant never consumes"
    );

    assert!(
        swarm::consume_grant_for_action(
            &store,
            &lease,
            &grant,
            &run,
            ActionClass::Semantic,
            Uuid::new_v4(),
            Utc::now()
        )
        .is_err(),
        "another owner session never consumes"
    );
}

#[test]
fn a_worker_claiming_computer_use_without_a_qualified_tier_is_rejected() {
    let mut spec = computer_graph();
    spec.workers[1].binding.computer_use_tier = ComputerUseTier::None;
    let error = spec
        .validate()
        .expect_err("an unqualified Computer Use worker must be rejected");
    assert!(error.message.contains("qualified tier"), "{error}");
}

// ---------------------------------------------------------------------------
// Mixed-provider attribution
// ---------------------------------------------------------------------------

#[test]
fn mixed_provider_receipts_are_attributed_per_exact_route() {
    let mut spec = base_spec();
    spec.budget.max_in_flight = 4;
    spec.workers = vec![
        worker(
            "w-x",
            WorkerRole::Investigate,
            &[WorkCapability::ReadWorkspace],
            "provider-x",
        ),
        worker(
            "w-y",
            WorkerRole::Investigate,
            &[WorkCapability::ReadWorkspace],
            "provider-y",
        ),
    ];
    spec.work = vec![
        item("x1", "w-x", &[], &[WorkCapability::ReadWorkspace]),
        item("y1", "w-y", &[], &[WorkCapability::ReadWorkspace]),
    ];
    let mut record = graph(spec, Uuid::new_v4(), "/tmp/ws");

    for (work, provider, effort, tokens) in [
        ("x1", "provider-x", EffortLevel::Medium, 100u64),
        ("y1", "provider-y", EffortLevel::High, 250u64),
    ] {
        let plan = swarm::plan_admissions(&record, 4, Utc::now());
        let intent = plan
            .intents
            .iter()
            .find(|i| i.work_id.to_string() == work)
            .cloned()
            .expect("intent");
        let lease_id = lease_id_for(&intent);
        let authority = authority_for(&record, &intent, route(provider, "synthetic-1", effort));
        swarm::issue_lease(
            &mut record,
            &intent,
            lease_id.clone(),
            &authority,
            None,
            Utc::now(),
        )
        .expect("lease");
        swarm::claim_spawn(&mut record, &lease_id, Utc::now()).expect("claim");
        swarm::acknowledge(&mut record, &lease_id, "child", Utc::now()).expect("ack");
        let attempt =
            swarm::record_attempt_admitted(&mut record, &lease_id, Utc::now()).expect("attempt");
        swarm::record_attempt_finished(
            &mut record,
            &attempt.attempt_id,
            SendCertainty::KnownAccepted,
            Some(200),
            Some(grokptah_agent_bridge::CompletionUsage {
                prompt_tokens: tokens / 2,
                completion_tokens: tokens / 2,
                total_tokens: tokens,
                requests: 1,
            }),
            Utc::now(),
        )
        .expect("finish");
    }

    let rows = swarm::project_attribution(&record);
    assert_eq!(rows.len(), 2, "one row per exact route");
    let keys: Vec<&str> = rows.iter().map(|r| r.attribution_key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "provider-x/loopback/synthetic-1@medium",
            "provider-y/loopback/synthetic-1@high"
        ]
    );
    assert_eq!(rows[0].tokens, 100);
    assert_eq!(rows[1].tokens, 250);
    assert_eq!(record.budget.tokens_used, 350);

    // Attribution carries no credential, no endpoint, and no keychain ref.
    let encoded = serde_json::to_string(&rows).expect("serialize");
    for forbidden in ["keychain", "127.0.0.1", "credential", "Bearer"] {
        assert!(
            !encoded.contains(forbidden),
            "attribution leaked {forbidden}: {encoded}"
        );
    }
}

#[test]
fn a_replayed_receipt_does_not_double_count_tokens() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    let attempt = swarm::record_attempt_admitted(&mut record, &lease, Utc::now()).expect("attempt");
    let usage = grokptah_agent_bridge::CompletionUsage {
        prompt_tokens: 10,
        completion_tokens: 10,
        total_tokens: 20,
        requests: 1,
    };
    for _ in 0..3 {
        swarm::record_attempt_finished(
            &mut record,
            &attempt.attempt_id,
            SendCertainty::KnownAccepted,
            Some(200),
            Some(usage.clone()),
            Utc::now(),
        )
        .expect("identical replay is a no-op");
    }
    assert_eq!(record.budget.tokens_used, 20);

    // A different outcome for the same attempt is a conflict, not a rewrite.
    assert!(
        swarm::record_attempt_finished(
            &mut record,
            &attempt.attempt_id,
            SendCertainty::KnownNotSent,
            None,
            None,
            Utc::now(),
        )
        .is_err(),
        "a conflicting completion must be refused"
    );
}

// ---------------------------------------------------------------------------
// Review quorum, disagreement, and Review/Discard
// ---------------------------------------------------------------------------

fn quorum_spec(required: u32) -> WorkGraphSpec {
    let mut spec = base_spec();
    spec.budget.max_in_flight = 8;
    spec.workers = vec![
        worker(
            "w-read",
            WorkerRole::Investigate,
            &[WorkCapability::ReadWorkspace],
            "loopback",
        ),
        worker(
            "w-rev",
            WorkerRole::Review,
            &[WorkCapability::ReadWorkspace],
            "loopback",
        ),
        worker(
            "w-syn",
            WorkerRole::Synthesize,
            &[WorkCapability::ReadWorkspace],
            "loopback",
        ),
    ];
    let mut synth = item(
        "synth",
        "w-syn",
        &["r1", "r2"],
        &[WorkCapability::ReadWorkspace],
    );
    synth.quorum = Some(QuorumGate {
        reviewers: vec![work_id("r1"), work_id("r2")],
        required_approvals: required,
    });
    spec.work = vec![
        item("r1", "w-rev", &[], &[WorkCapability::ReadWorkspace]),
        item("r2", "w-rev", &[], &[WorkCapability::ReadWorkspace]),
        synth,
    ];
    spec
}

fn settle_reviewer(record: &mut WorkGraphRecord, id: &str, verdict: ReviewVerdict) {
    let plan = swarm::plan_admissions(record, 8, Utc::now());
    let intent = plan
        .intents
        .iter()
        .find(|i| i.work_id.to_string() == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} should be admissible"));
    let lease_id = lease_id_for(&intent);
    let authority = authority_for(
        record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    swarm::issue_lease(
        record,
        &intent,
        lease_id.clone(),
        &authority,
        None,
        Utc::now(),
    )
    .expect("lease");
    swarm::claim_spawn(record, &lease_id, Utc::now()).expect("claim");
    swarm::acknowledge(record, &lease_id, "child", Utc::now()).expect("ack");
    swarm::settle(
        record,
        &lease_id,
        &WorkOutcome::succeeded().with_verdict(verdict),
        Utc::now(),
    )
    .expect("settle");
}

#[test]
fn a_synthesis_item_without_a_quorum_gate_is_invalid() {
    let mut spec = quorum_spec(2);
    spec.work[2].quorum = None;
    let error = spec
        .validate()
        .expect_err("a missing gate must be rejected");
    assert!(error.message.contains("quorum gate"), "{error}");
}

#[test]
fn a_gate_naming_a_non_reviewer_or_an_undepended_reviewer_is_invalid() {
    let mut spec = quorum_spec(2);
    spec.work[2].quorum = Some(QuorumGate {
        reviewers: vec![work_id("r1"), work_id("synth")],
        required_approvals: 1,
    });
    assert!(spec.validate().is_err(), "a non-review reviewer is invalid");

    let mut spec = quorum_spec(2);
    spec.work[2].depends_on = vec![work_id("r1")];
    assert!(
        spec.validate().is_err(),
        "a gate must depend on the reviewers it counts"
    );

    let mut spec = quorum_spec(3);
    assert!(
        spec.validate().is_err(),
        "requiring more approvals than reviewers is invalid"
    );
    spec.work[2].quorum = Some(QuorumGate {
        reviewers: vec![work_id("r1"), work_id("r2")],
        required_approvals: 0,
    });
    assert!(spec.validate().is_err(), "zero approvals is invalid");
}

#[test]
fn reviewers_that_disagree_withhold_the_gate_even_when_both_succeeded() {
    let mut record = graph(quorum_spec(2), Uuid::new_v4(), "/tmp/ws");
    settle_reviewer(&mut record, "r1", ReviewVerdict::Approve);
    settle_reviewer(&mut record, "r2", ReviewVerdict::Reject);

    assert_eq!(
        record.work_record(&work_id("r1")).map(|r| r.state),
        Some(WorkState::Succeeded)
    );
    assert_eq!(
        record.work_record(&work_id("r2")).map(|r| r.state),
        Some(WorkState::Succeeded),
        "a reviewer that rejects still ran successfully"
    );
    assert_eq!(
        record.work_record(&work_id("synth")).map(|r| r.state),
        Some(WorkState::Blocked),
        "the gate can no longer be met, so synthesis is blocked"
    );
    let plan = swarm::plan_admissions(&record, 8, Utc::now());
    assert!(plan
        .intents
        .iter()
        .all(|i| i.work_id.to_string() != "synth"));
}

#[test]
fn a_met_quorum_opens_the_gate() {
    let mut record = graph(quorum_spec(1), Uuid::new_v4(), "/tmp/ws");
    settle_reviewer(&mut record, "r1", ReviewVerdict::Approve);
    settle_reviewer(&mut record, "r2", ReviewVerdict::Reject);
    assert_eq!(
        record.work_record(&work_id("synth")).map(|r| r.state),
        Some(WorkState::Ready),
        "one approval satisfies a quorum of one"
    );
}

#[test]
fn a_completed_review_must_report_a_verdict_and_only_a_review_may() {
    let mut record = graph(quorum_spec(1), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 8, Utc::now());
    let intent = plan
        .intents
        .iter()
        .find(|i| i.work_id.to_string() == "r1")
        .cloned()
        .expect("intent");
    let lease_id = lease_id_for(&intent);
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    swarm::issue_lease(
        &mut record,
        &intent,
        lease_id.clone(),
        &authority,
        None,
        Utc::now(),
    )
    .expect("lease");
    swarm::claim_spawn(&mut record, &lease_id, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease_id, "child", Utc::now()).expect("ack");

    let error = swarm::settle(
        &mut record,
        &lease_id,
        &WorkOutcome::succeeded(),
        Utc::now(),
    )
    .expect_err("a review must report a verdict");
    assert!(error.message.contains("verdict"), "{error}");

    // A non-review item reporting a verdict is equally refused.
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child", Utc::now()).expect("ack");
    assert!(
        swarm::settle(
            &mut record,
            &lease,
            &WorkOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
            Utc::now()
        )
        .is_err(),
        "only a review item may report a verdict"
    );
}

#[test]
fn discarding_a_reviewed_item_is_terminal_and_never_counted_as_success() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child", Utc::now()).expect("ack");
    swarm::settle(&mut record, &lease, &WorkOutcome::succeeded(), Utc::now()).expect("settle");

    let state = swarm::review_work(
        &mut record,
        &work_id("a"),
        ReviewDecision::Discard,
        Utc::now(),
    )
    .expect("discard");
    assert_eq!(state, WorkState::Discarded);
    assert!(state.is_settled());

    let status = swarm::project_status(&record, None, &BoundOnlyRedactor);
    assert_eq!(status.discarded, 1);
    assert_eq!(
        status.succeeded, 0,
        "a discard is never counted as a success"
    );

    // An item that never succeeded cannot be discarded.
    assert!(
        swarm::review_work(
            &mut record,
            &work_id("b"),
            ReviewDecision::Discard,
            Utc::now()
        )
        .is_err(),
        "there is nothing to discard"
    );
}

#[test]
fn keeping_a_reviewed_item_leaves_it_succeeded() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child", Utc::now()).expect("ack");
    swarm::settle(&mut record, &lease, &WorkOutcome::succeeded(), Utc::now()).expect("settle");
    let state = swarm::review_work(&mut record, &work_id("a"), ReviewDecision::Keep, Utc::now())
        .expect("keep");
    assert_eq!(state, WorkState::Succeeded);
}

// ---------------------------------------------------------------------------
// Evidence redaction and secret-free projections
// ---------------------------------------------------------------------------

#[test]
fn evidence_is_redacted_and_bounded_before_it_is_projected() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child", Utc::now()).expect("ack");

    let secret = "sk-super-secret-token-value";

    // Evidence larger than the durable bound is refused at ingest rather than
    // stored and trimmed later.
    assert!(
        swarm::settle(
            &mut record,
            &lease,
            &WorkOutcome::succeeded()
                .with_evidence(vec![EvidenceEntry::new("oversized", "x".repeat(4_000))]),
            Utc::now(),
        )
        .is_err(),
        "evidence beyond the durable bound must be refused"
    );

    swarm::settle(
        &mut record,
        &lease,
        &WorkOutcome::succeeded().with_evidence(vec![
            EvidenceEntry::new("finding", format!("the worker printed {secret} to the log")),
            // Within the durable bound, but well past the projection bound.
            EvidenceEntry::new("long", "x".repeat(1_500)),
        ]),
        Utc::now(),
    )
    .expect("settle");

    // The caller's redactor is what strips registered secrets, exactly as it
    // does for the durable journal.
    let redactor = move |text: &str, max: usize| {
        let replaced = text.replace(secret, "[redacted]");
        let mut end = max.min(replaced.len());
        while end > 0 && !replaced.is_char_boundary(end) {
            end -= 1;
        }
        replaced[..end].to_string()
    };
    let rows = swarm::project_evidence(&record, &redactor);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert!(!row.detail.contains(secret), "evidence leaked a secret");
        assert!(
            row.detail.len() <= swarm::projection::MAX_PROJECTED_TEXT_BYTES,
            "evidence must be bounded"
        );
    }
    assert!(rows[0].detail.contains("[redacted]"));
    // Deterministic order.
    assert_eq!(rows[0].work_id, "a");
    assert!(rows[0].label <= rows[1].label);
}

#[test]
fn evidence_bounds_cut_on_a_character_boundary() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::acknowledge(&mut record, &lease, "child", Utc::now()).expect("ack");
    // Multi-byte characters straddling the bound must not be split.
    let wide = "é".repeat(600);
    swarm::settle(
        &mut record,
        &lease,
        &WorkOutcome::succeeded().with_evidence(vec![EvidenceEntry::new("wide", wide)]),
        Utc::now(),
    )
    .expect("settle");
    let rows = swarm::project_evidence(&record, &BoundOnlyRedactor);
    assert_eq!(rows.len(), 1);
    // Reaching this line at all proves no panic; assert the bound too.
    assert!(rows[0].detail.len() <= swarm::projection::MAX_PROJECTED_TEXT_BYTES);
    assert!(rows[0].detail.chars().all(|c| c == 'é'));
}

#[test]
fn no_projection_can_carry_a_credential_reference() {
    let mut record = graph(computer_graph(), Uuid::new_v4(), "/tmp/ws");
    let plan = swarm::plan_admissions(&record, 4, Utc::now());
    let intent = plan.intents[0].clone();
    let session = record.session_id;
    let (run, grant) = computer_fixture(session);
    let lease_id = lease_id_for(&intent);
    let binding = swarm::bind_grant(&grant, &run, &lease_id, &intent.attempt_id).expect("bind");
    let authority = authority_for(
        &record,
        &intent,
        route("loopback", "synthetic-1", EffortLevel::Medium),
    );
    swarm::issue_lease(
        &mut record,
        &intent,
        lease_id,
        &authority,
        Some(binding),
        Utc::now(),
    )
    .expect("lease");

    let projection = swarm::project_graph(&record, None, &BoundOnlyRedactor);
    let encoded = serde_json::to_string(&projection).expect("serialize");
    for forbidden in [
        "keychain://",
        "credentialRef",
        "credentialFingerprint",
        "127.0.0.1",
        "baseUrl",
        "bindingHash",
        "grantId",
        "authorityId",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "projection leaked {forbidden}:\n{encoded}"
        );
    }
    // The lease row says only *that* a grant is bound.
    assert!(encoded.contains("computerUseBound"));

    let desktop = swarm::project_desktop(&projection.status);
    let desktop_encoded = serde_json::to_string(&desktop).expect("serialize");
    for forbidden in ["keychain", "grant", "authority", "lease", "credential"] {
        assert!(
            !desktop_encoded.to_lowercase().contains(forbidden),
            "desktop DTO leaked {forbidden}: {desktop_encoded}"
        );
    }
}

#[test]
fn an_uncertain_graph_raises_operator_attention_in_its_projection() {
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);
    swarm::claim_spawn(&mut record, &lease, Utc::now()).expect("claim");
    swarm::recover(&mut record, Utc::now());

    let status = swarm::project_status(&record, Some(AdmissionBlock::None), &BoundOnlyRedactor);
    assert!(status.needs_operator_attention);
    assert_eq!(status.uncertain, 1);
    assert_eq!(
        status.admission_block, None,
        "None is not surfaced as a block"
    );

    let desktop = swarm::project_desktop(&status);
    assert!(desktop.needs_operator_attention);
}

// ---------------------------------------------------------------------------
// Genuinely concurrent workers
// ---------------------------------------------------------------------------

#[test]
fn many_threads_racing_one_lease_produce_exactly_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let mut record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    let (_, lease) = issue_first(&mut record, 4);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for worker in 0..16 {
        let store = store.clone();
        let lease = lease.clone();
        let barrier = barrier.clone();
        let winners = winners.clone();
        handles.push(std::thread::spawn(move || {
            // Release every thread into the claim at the same moment.
            barrier.wait();
            let outcome = store
                .claim_lease_spawn(&lease, &format!("worker-{worker}"))
                .expect("claim");
            if outcome == ClaimOutcome::Won {
                winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for handle in handles {
        handle.join().expect("thread joins");
    }
    assert_eq!(
        winners.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one of sixteen concurrent workers may spawn"
    );
    assert!(store.lease_spawn_holder(&lease).expect("holder").is_some());
}

#[test]
fn many_threads_racing_one_grant_consume_it_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let grant_id = swarm::GrantId::parse("grant-race").expect("grant id");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for holder in 0..12 {
        let store = store.clone();
        let grant_id = grant_id.clone();
        let barrier = barrier.clone();
        let winners = winners.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            if store
                .consume_grant(&grant_id, 3, &format!("holder-{holder}"))
                .expect("consume")
                == ClaimOutcome::Won
            {
                winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for handle in handles {
        handle.join().expect("thread joins");
    }
    assert_eq!(
        winners.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a grant is consumed by exactly one holder per control epoch"
    );
    // A different control epoch is a different key, not a contested one.
    assert_eq!(
        store
            .consume_grant(&grant_id, 4, "post-takeover")
            .expect("consume"),
        ClaimOutcome::Won
    );
}

#[test]
fn concurrent_compare_and_swap_writers_leave_exactly_one_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let record = graph(base_spec(), Uuid::new_v4(), "/tmp/ws");
    store.create_graph(&record).expect("create");
    let observed = store
        .load_graph(&record.graph_id)
        .expect("load")
        .expect("some");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let observed = observed.clone();
        let barrier = barrier.clone();
        let commits = commits.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            if store
                .compare_and_swap(observed.revision, &observed, Utc::now())
                .is_ok()
            {
                commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for handle in handles {
        handle.join().expect("thread joins");
    }
    let committed = commits.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        committed, 1,
        "all eight writers observed the same revision; exactly one may commit"
    );
    let final_record = store
        .load_graph(&record.graph_id)
        .expect("load")
        .expect("some");
    assert_eq!(
        final_record.revision,
        observed.revision + 1,
        "a lost writer must not advance the revision"
    );
}
