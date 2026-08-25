//! Deterministic validation: malformed graphs and fail-closed authority.

mod support;

use grokptah_swarm_control_plane::{
    IsolationRequirement, ProviderCatalog, QuorumRule, SubagentCapabilityMode, SwarmController,
    SwarmErrorCode, TaskKind, TaskOutcome, WorkerCapability, validate_swarm_spec,
};
use support::*;

fn expect_rejected(spec: grokptah_swarm_control_plane::SwarmSpec, code: SwarmErrorCode) -> String {
    let error = validate_swarm_spec(&spec).expect_err("specification must be rejected");
    assert_eq!(error.code, code, "unexpected error code: {error}");
    // Construction must refuse the same specification, not just validation.
    let constructed = SwarmController::new(spec, at(0));
    assert!(
        constructed.is_err(),
        "an invalid specification must never build a controller"
    );
    error.message
}

#[test]
fn the_reference_graph_validates() {
    let spec = diamond_spec(QuorumRule::Unanimous);
    validate_swarm_spec(&spec).expect("the fixture graph is valid");
}

#[test]
fn duplicate_task_ids_are_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks
        .push(work_task("t-a", "impl-grok", &["t-root"], 0));
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("more than once"), "{message}");
}

#[test]
fn duplicate_worker_ids_are_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.workers.push(implementer());
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("more than once"), "{message}");
}

#[test]
fn a_missing_dependency_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks
        .push(work_task("t-orphan", "impl-grok", &["t-absent"], 0));
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("does not declare"), "{message}");
}

#[test]
fn a_self_dependency_is_rejected() {
    let mut spec = single_task_spec();
    spec.tasks[0].dependencies = vec![task_id("t-only")];
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("depends on itself"), "{message}");
}

#[test]
fn a_repeated_dependency_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks[1].dependencies = vec![task_id("t-root"), task_id("t-root")];
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("repeats a dependency"), "{message}");
}

#[test]
fn a_dependency_cycle_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    // t-root now depends on its own descendant, closing the diamond into a loop.
    spec.tasks[0].dependencies = vec![task_id("t-synth")];
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("cycle"), "{message}");
}

#[test]
fn a_long_dependency_cycle_is_rejected() {
    let mut spec = single_task_spec();
    spec.tasks = (0..12)
        .map(|index| {
            let id = format!("t-{index}");
            let previous = format!("t-{}", (index + 11) % 12);
            work_task(&id, "impl-grok", &[previous.as_str()], 0)
        })
        .collect();
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("cycle"), "{message}");
}

#[test]
fn exceeding_the_fan_out_bound_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.admission.max_fan_out = 1;
    // t-root already has two direct dependents.
    let message = expect_rejected(spec, SwarmErrorCode::BoundExceeded);
    assert!(message.contains("maxFanOut"), "{message}");
}

#[test]
fn an_out_of_range_concurrency_bound_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.admission.max_in_flight = 0;
    expect_rejected(spec, SwarmErrorCode::BoundExceeded);

    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.admission.max_in_flight = 9_999;
    expect_rejected(spec, SwarmErrorCode::BoundExceeded);
}

#[test]
fn an_empty_task_list_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks.clear();
    expect_rejected(spec, SwarmErrorCode::BoundExceeded);
}

#[test]
fn a_task_naming_an_undeclared_worker_is_rejected() {
    let mut spec = single_task_spec();
    spec.tasks[0].worker_id = worker_id("no-such-worker");
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("does not declare"), "{message}");
}

#[test]
fn a_blank_or_control_bearing_objective_is_rejected() {
    let mut spec = single_task_spec();
    spec.objective = "   ".to_string();
    expect_rejected(spec, SwarmErrorCode::InvalidSpec);

    let mut spec = single_task_spec();
    spec.objective = "escape\u{1b}[2J".to_string();
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("control characters"), "{message}");
}

// ── fail-closed provider, model, and capability admission ────────────────

#[test]
fn an_empty_catalog_admits_nothing() {
    let mut spec = single_task_spec();
    spec.catalog = ProviderCatalog::default();
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("does not measure"), "{message}");
}

#[test]
fn an_unmeasured_model_is_rejected() {
    let mut spec = single_task_spec();
    spec.workers[0].model = model("grok-code-fast-2");
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("does not measure"), "{message}");
}

#[test]
fn an_unmeasured_provider_is_rejected() {
    let mut spec = single_task_spec();
    spec.workers[0].provider = provider("unlisted");
    expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
}

#[test]
fn an_unmeasured_role_is_rejected() {
    let mut spec = single_task_spec();
    // Grok is measured for implementer and explorer work, never for review.
    spec.workers[0].role = grokptah_swarm_control_plane::WorkerRole::Reviewer;
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("requested role"), "{message}");
}

#[test]
fn an_unmeasured_capability_is_rejected() {
    let mut spec = single_task_spec();
    spec.workers[0]
        .capabilities
        .insert(WorkerCapability::ComputerUseLeased);
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("requested capability"), "{message}");
}

#[test]
fn an_unmeasured_capability_mode_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    // The Claude reviewer entry is measured for read-only work only. Pair the
    // unmeasured mode with worktree isolation so the shape rules pass and the
    // catalog check is the one that refuses it.
    spec.workers[1].capability_mode = SubagentCapabilityMode::Execute;
    spec.workers[1].isolation = IsolationRequirement::Worktree;
    spec.workers[1]
        .capabilities
        .insert(WorkerCapability::ExecuteInWorktree);
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("capability mode 'execute'"), "{message}");
}

// ── isolation contract ───────────────────────────────────────────────────

#[test]
fn a_mutating_worker_must_require_a_worktree() {
    let mut spec = single_task_spec();
    spec.workers[0].isolation = IsolationRequirement::SharedReadOnly;
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("worktree isolation"), "{message}");
}

#[test]
fn a_non_read_only_worker_must_require_a_worktree() {
    let mut spec = single_task_spec();
    spec.workers[0].capabilities = [WorkerCapability::ReadWorkspace].into_iter().collect();
    spec.workers[0].isolation = IsolationRequirement::SharedReadOnly;
    // Capabilities alone no longer mutate, but the capability mode still does.
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("worktree isolation"), "{message}");
}

#[test]
fn a_read_only_mode_cannot_hold_mutating_capabilities() {
    let mut spec = single_task_spec();
    spec.workers[0].capability_mode = SubagentCapabilityMode::ReadOnly;
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("read-only capability mode"), "{message}");
}

#[test]
fn isolation_projects_onto_the_existing_subagent_wire_enum() {
    use grokptah_swarm_control_plane::SubagentIsolationMode;
    assert_eq!(
        IsolationRequirement::Worktree.as_subagent_isolation(),
        SubagentIsolationMode::Worktree
    );
    assert_eq!(
        IsolationRequirement::SharedReadOnly.as_subagent_isolation(),
        SubagentIsolationMode::None
    );
}

// ── no browser or raw-host authority is expressible ──────────────────────

#[test]
fn the_capability_vocabulary_has_no_browser_or_raw_host_variant() {
    for denied in [
        "browser",
        "browser_automation",
        "browser_use",
        "raw_host",
        "host_shell",
        "execute_on_host",
        "network_egress",
        "all",
    ] {
        let json = format!("\"{denied}\"");
        let parsed = serde_json::from_str::<WorkerCapability>(&json);
        assert!(
            parsed.is_err(),
            "'{denied}' must not deserialize into a worker capability"
        );
    }
}

#[test]
fn the_capability_vocabulary_is_exactly_the_measured_set() {
    let expected = [
        "read_workspace",
        "write_workspace",
        "execute_in_worktree",
        "review",
        "synthesize",
        "computer_use_leased",
    ];
    for name in expected {
        let json = format!("\"{name}\"");
        serde_json::from_str::<WorkerCapability>(&json)
            .unwrap_or_else(|error| panic!("'{name}' must deserialize: {error}"));
    }
}

#[test]
fn unknown_fields_in_a_specification_are_refused() {
    let json = r#"{
        "workerId": "impl-grok",
        "provider": "grok",
        "model": "grok-code-fast-1",
        "role": "implementer",
        "capabilityMode": "read-write",
        "capabilities": ["read_workspace"],
        "isolation": "worktree",
        "hostShell": true
    }"#;
    let parsed = serde_json::from_str::<grokptah_swarm_control_plane::WorkerSpec>(json);
    assert!(
        parsed.is_err(),
        "an unknown worker field must fail closed rather than being ignored"
    );
}

// ── Computer Use contract ────────────────────────────────────────────────

#[test]
fn a_computer_use_task_needs_a_worker_measured_for_it() {
    let mut spec = single_task_spec();
    spec.tasks[0].requires_computer_use = true;
    spec.tasks[0].computer_use = Some(computer_use_requirement());
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("leased Computer Use"), "{message}");
}

#[test]
fn a_computer_use_task_validates_against_a_measured_worker() {
    let mut spec = single_task_spec();
    spec.workers = vec![computer_use_worker()];
    spec.tasks[0].worker_id = worker_id("cu-cursor");
    spec.tasks[0].requires_computer_use = true;
    spec.tasks[0].computer_use = Some(computer_use_requirement());
    spec.tasks[0]
        .capabilities
        .insert(WorkerCapability::ComputerUseLeased);
    validate_swarm_spec(&spec).expect("a leased Computer Use worker is admissible");
}

// ── review and synthesis structure ───────────────────────────────────────

#[test]
fn a_review_gate_on_a_non_synthesis_task_is_rejected() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    let gate = spec.tasks[5].review_gate.clone();
    spec.tasks[3].review_gate = gate;
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("not a synthesis task"), "{message}");
}

#[test]
fn a_gate_must_name_review_tasks() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks[5].dependencies = vec![task_id("t-a"), task_id("t-review-b")];
    spec.tasks[5].review_gate = Some(grokptah_swarm_control_plane::ReviewGate {
        reviewers: vec![task_id("t-a"), task_id("t-review-b")],
        quorum: QuorumRule::Unanimous,
    });
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("not a review task"), "{message}");
}

#[test]
fn a_gate_must_depend_on_every_reviewer_it_names() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks[5].dependencies = vec![task_id("t-review-a")];
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("without depending on it"), "{message}");
}

#[test]
fn a_quorum_larger_than_the_reviewer_pool_is_rejected() {
    let spec = diamond_spec(QuorumRule::AtLeast { approvals: 3 });
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(
        message.contains("exceed the number of reviewers"),
        "{message}"
    );
}

#[test]
fn a_task_kind_must_match_its_worker_role() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    // Point a review node at the implementer.
    spec.tasks[3].worker_id = worker_id("impl-grok");
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(
        message.contains("cannot perform that task kind"),
        "{message}"
    );
}

#[test]
fn review_and_synthesis_tasks_require_their_capability() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.workers[1]
        .capabilities
        .remove(&WorkerCapability::Review);
    expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);

    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.workers[2]
        .capabilities
        .remove(&WorkerCapability::Synthesize);
    expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);

    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.workers[0].capability_mode = SubagentCapabilityMode::All;
    expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
}

#[test]
fn dispatch_intent_uses_the_task_capability_set() {
    let mut spec = single_task_spec();
    spec.tasks[0].capabilities = [WorkerCapability::ReadWorkspace].into_iter().collect();
    spec.tasks[0].capability_mode = SubagentCapabilityMode::ReadOnly;
    let swarm = SwarmController::new(spec, at(0)).expect("least-privilege task is valid");
    let intent = swarm
        .plan_dispatches(at(1))
        .into_iter()
        .next()
        .expect("root is ready");
    assert_eq!(
        intent.capabilities,
        [WorkerCapability::ReadWorkspace].into_iter().collect()
    );
    assert_eq!(intent.capability_mode, SubagentCapabilityMode::ReadOnly);
}

#[test]
fn a_read_only_worker_cannot_run_an_execute_task() {
    let mut spec = single_task_spec();
    spec.workers[0].capability_mode = SubagentCapabilityMode::ReadOnly;
    spec.workers[0].capabilities = [WorkerCapability::ReadWorkspace].into_iter().collect();
    spec.workers[0].isolation = IsolationRequirement::SharedReadOnly;
    spec.tasks[0].capability_mode = SubagentCapabilityMode::Execute;
    spec.tasks[0].capabilities = [WorkerCapability::ReadWorkspace].into_iter().collect();
    expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);

    let mut spec = single_task_spec();
    spec.workers[0].capability_mode = SubagentCapabilityMode::ReadOnly;
    spec.workers[0].capabilities = [WorkerCapability::ReadWorkspace].into_iter().collect();
    spec.workers[0].isolation = IsolationRequirement::Worktree;
    spec.tasks[0].capability_mode = SubagentCapabilityMode::Execute;
    spec.tasks[0].capabilities = [
        WorkerCapability::ReadWorkspace,
        WorkerCapability::ExecuteInWorktree,
    ]
    .into_iter()
    .collect();
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("beyond the worker"), "{message}");
}

#[test]
fn a_read_write_task_must_declare_write_workspace() {
    let mut spec = single_task_spec();
    spec.tasks[0].capabilities = [
        WorkerCapability::ReadWorkspace,
        WorkerCapability::ExecuteInWorktree,
    ]
    .into_iter()
    .collect();
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("WriteWorkspace"), "{message}");
}

#[test]
fn a_work_task_must_declare_its_minimum_capability() {
    let mut spec = single_task_spec();
    spec.tasks[0].capabilities = [
        WorkerCapability::WriteWorkspace,
        WorkerCapability::ExecuteInWorktree,
    ]
    .into_iter()
    .collect();
    let message = expect_rejected(spec, SwarmErrorCode::CapabilityNotGranted);
    assert!(message.contains("task-kind capability"), "{message}");
}

#[test]
fn a_synthesis_task_must_declare_a_review_quorum() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks[5].review_gate = None;
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("review quorum"), "{message}");
}

#[test]
fn runtime_payloads_reject_unknown_fields() {
    let parsed =
        serde_json::from_str::<TaskOutcome>(r#"{"result":"succeeded","unexpected":"must fail"}"#);
    assert!(parsed.is_err(), "terminal payloads must fail closed");
}

#[test]
fn a_catalog_entry_must_not_repeat_a_capability_mode() {
    let mut spec = single_task_spec();
    spec.catalog.entries[0]
        .capability_modes
        .push(SubagentCapabilityMode::ReadWrite);
    let message = expect_rejected(spec, SwarmErrorCode::InvalidSpec);
    assert!(message.contains("repeat a capability mode"), "{message}");
}

#[test]
fn quorum_arithmetic_is_explicit() {
    assert_eq!(QuorumRule::Unanimous.required_approvals(3), 3);
    assert_eq!(QuorumRule::Majority.required_approvals(3), 2);
    assert_eq!(QuorumRule::Majority.required_approvals(4), 3);
    assert_eq!(
        QuorumRule::AtLeast { approvals: 1 }.required_approvals(4),
        1
    );
}

#[test]
fn task_kinds_round_trip_on_the_wire() {
    for (kind, wire) in [
        (TaskKind::Work, "\"work\""),
        (TaskKind::Review, "\"review\""),
        (TaskKind::Synthesis, "\"synthesis\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).expect("serializes"), wire);
        assert_eq!(
            serde_json::from_str::<TaskKind>(wire).expect("deserializes"),
            kind
        );
    }
}
