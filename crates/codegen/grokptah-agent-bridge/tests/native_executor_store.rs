use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    assemble_managed_run_input, intersect_run_bounds, managed_execution_eligible,
    select_relevant_managed_messages, AssignmentStatus, AttemptState,
    ManagedExecutionBudgetProfile, ManagedExecutionIntent, ManagedExecutionPolicy,
    ManagedExecutorKind, ManagedFinalizationOutcome, ManagedFinalizationStage, ManagedIntentState,
    ManagedRetryCause, ManagedWorkMode, MessageKind, OrchErrorCode, OrchStore, RunBounds,
    RunExecutionMode, RunRecord, RunState, WorkItem, WorkMessage, WorkPolicy, WorkProgress,
    WorkResult, WorkState, MANAGED_EXECUTION_SCHEMA_VERSION,
};
use grokptah_agent_bridge::{AgentRecord, AgentState};
use tempfile::tempdir;
use uuid::Uuid;

fn agent(id: &str, workspace: &str, session_id: Uuid) -> AgentRecord {
    let now = Utc::now();
    AgentRecord {
        agent_id: id.into(),
        owner_principal_id: None,
        session_id,
        lane_ids: vec![session_id],
        lane_associations: Vec::new(),
        workspace: workspace.into(),
        model: "grok".into(),
        spec: None,
        state: AgentState::Waiting,
        current_run_id: None,
        last_run_id: None,
        last_lane_id: Some(session_id),
        latest_checkpoint_id: None,
        continuation_ordinal: 0,
        created_at: now,
        updated_at: now,
    }
}

fn accepted_work(session_id: Uuid, workspace: &str, agent_id: &str) -> WorkItem {
    let mut item = WorkItem::new(
        "native",
        "Execute natively",
        session_id,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    item.assigned_agent_id = Some(agent_id.into());
    item.assignment_status = AssignmentStatus::Accepted;
    item
}

#[test]
fn legacy_agent_spec_deserializes_as_manual_only() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let loaded = store.load_agent("worker-a").unwrap().unwrap();
    let spec = loaded.current_spec().unwrap();
    assert!(!spec.managed_execution.enabled);
}

#[test]
fn enabling_managed_execution_requires_a_spec_revision() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let first = store.load_agent("worker-a").unwrap().unwrap();
    let before = first.current_spec().unwrap().revision;
    let updated = store
        .revise_agent_spec("worker-a", "operator", |spec| {
            spec.managed_execution.enabled = true;
            spec.managed_execution.bounds.max_total_tokens = Some(8_000);
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(updated.current_spec().unwrap().managed_execution.enabled);
    assert_eq!(updated.current_spec().unwrap().revision, before + 1);
}

#[test]
fn manual_only_and_foreign_agents_are_not_eligible() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let other = Uuid::new_v4();
    store
        .save_agent(&agent("local", "/tmp/ws", session))
        .unwrap();
    store
        .save_agent(&agent("foreign", "/tmp/other", other))
        .unwrap();
    let local = store.load_agent("local").unwrap().unwrap();
    let spec = local.current_spec().unwrap().clone();
    let work = accepted_work(session, "/tmp/ws", "local");
    let ceiling = RunBounds::default();
    assert!(managed_execution_eligible(&work, &local, &spec, &[], 0, &ceiling).is_err());
    let mut enabled = spec.clone();
    enabled.managed_execution.enabled = true;
    enabled.managed_execution.bounds.max_total_tokens = Some(4_000);
    assert!(managed_execution_eligible(&work, &local, &enabled, &[], 0, &ceiling).is_ok());
    let foreign = store.load_agent("foreign").unwrap().unwrap();
    assert_eq!(
        managed_execution_eligible(&work, &foreign, &enabled, &[], 0, &ceiling)
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );
    let mut forbidden = work.clone();
    forbidden.policy.managed_execution = ManagedWorkMode::Forbid;
    assert!(managed_execution_eligible(&forbidden, &local, &enabled, &[], 0, &ceiling).is_err());
}

#[test]
fn managed_execution_requires_the_current_bound_authorization() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    store
        .revise_agent_spec("worker-a", "operator", |spec| {
            spec.managed_execution.enabled = true;
            spec.managed_execution.requires_approval_before_execution = true;
            spec.managed_execution.bounds.max_total_tokens = Some(4_000);
            Ok(())
        })
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let (authorized, decision) = store
        .authorize_work_execution(
            &item.work_id,
            "operator",
            None,
            "authorize one managed attempt",
            Some(item.revision),
            Utc::now(),
        )
        .unwrap();
    let worker = store.load_agent("worker-a").unwrap().unwrap();
    let spec = worker.current_spec().unwrap().clone();
    let ceiling = RunBounds::default();
    assert_eq!(
        authorized.last_decision_id.as_deref(),
        Some(decision.decision_id.as_str())
    );
    assert_eq!(
        decision.work_revision.map(|revision| revision + 1),
        Some(authorized.revision)
    );
    assert_eq!(
        decision.assigned_agent_id.as_deref(),
        authorized.assigned_agent_id.as_deref()
    );
    assert_eq!(decision.policy_revision, Some(spec.revision));
    let eligible = managed_execution_eligible(
        &authorized,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    );
    assert!(eligible.is_ok(), "{eligible:?}");

    let mut later_revision = authorized.clone();
    later_revision.bump();
    assert!(managed_execution_eligible(
        &later_revision,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_err());

    let mut detached = authorized.clone();
    detached.last_decision_id = None;
    assert!(managed_execution_eligible(
        &detached,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_err());

    let revised = store
        .revise_agent_spec("worker-a", "operator", |spec| {
            spec.default_run_bounds.max_prompt_bytes =
                spec.default_run_bounds.max_prompt_bytes.saturating_sub(1);
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(managed_execution_eligible(
        &authorized,
        &revised,
        revised.current_spec().unwrap(),
        &[decision],
        0,
        &ceiling,
    )
    .is_err());
}

#[test]
fn grok_managed_execution_requires_the_strict_self_host_authority_envelope() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    store
        .revise_agent_spec("worker-a", "operator", |spec| {
            spec.managed_execution.enabled = true;
            spec.managed_execution.executor = ManagedExecutorKind::GrokBuildIsolatedReview;
            spec.managed_execution.budget_profile = Some(ManagedExecutionBudgetProfile::Economy);
            spec.managed_execution.requires_approval_before_execution = true;
            spec.managed_execution.retry_eligible = false;
            spec.managed_execution.bounds.max_total_tokens = Some(4_000);
            Ok(())
        })
        .unwrap();
    let mut item = accepted_work(session, "/tmp/ws", "worker-a");
    item.policy.allowed_files = vec!["src/lib.rs".into()];
    item.policy.retry.max_attempts = 1;
    item.policy.retry.retry_failed = false;
    item.policy.retry.retry_expired = false;
    item.source_manager_plan_id = Some("plan-1".into());
    item.source_manager_step_id = Some("step-1".into());
    store.save_work_item(&item).unwrap();
    let (authorized, decision) = store
        .authorize_work_execution(
            &item.work_id,
            "operator",
            None,
            "authorize one isolated Grok review",
            Some(item.revision),
            Utc::now(),
        )
        .unwrap();
    let worker = store.load_agent("worker-a").unwrap().unwrap();
    let spec = worker.current_spec().unwrap().clone();
    let ceiling = RunBounds::default();
    assert!(managed_execution_eligible(
        &authorized,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_ok());

    let mut missing_scope = authorized.clone();
    missing_scope.policy.allowed_files.clear();
    assert!(managed_execution_eligible(
        &missing_scope,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_err());

    let mut missing_plan = authorized.clone();
    missing_plan.source_manager_plan_id = None;
    missing_plan.source_manager_step_id = None;
    assert!(managed_execution_eligible(
        &missing_plan,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_err());

    let mut retryable = authorized.clone();
    retryable.policy.retry.max_attempts = 2;
    retryable.policy.retry.retry_failed = true;
    assert!(managed_execution_eligible(
        &retryable,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_err());

    let mut no_profile = spec.clone();
    no_profile.managed_execution.budget_profile = None;
    assert!(managed_execution_eligible(
        &authorized,
        &worker,
        &no_profile,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    )
    .is_err());
}

#[test]
fn grok_budget_profiles_change_cost_bounds_not_authority() {
    let economy = ManagedExecutionBudgetProfile::Economy.limits();
    let high = ManagedExecutionBudgetProfile::HighAssurance.limits();
    assert!(economy.max_prompt_bytes < high.max_prompt_bytes);
    assert!(economy.max_turns < high.max_turns);
    assert!(economy.max_duration_ms < high.max_duration_ms);
    assert!(economy.max_output_bytes < high.max_output_bytes);

    let legacy = ManagedExecutionPolicy::default();
    assert_eq!(legacy.executor, ManagedExecutorKind::NativeRun);
    assert_eq!(legacy.budget_profile, None);
    assert!(legacy.validate().is_ok());

    let mut invalid_native = legacy;
    invalid_native.budget_profile = Some(ManagedExecutionBudgetProfile::Economy);
    assert!(invalid_native.validate().is_err());
}

#[test]
fn bounds_intersection_never_widens() {
    let server = RunBounds {
        max_prompt_bytes: 10_000,
        max_rounds: 10,
        max_duration_ms: 60_000,
        max_total_tokens: Some(20_000),
    };
    let agent = RunBounds {
        max_prompt_bytes: 8_000,
        max_rounds: 20,
        max_duration_ms: 30_000,
        max_total_tokens: Some(50_000),
    };
    let work = RunBounds {
        max_prompt_bytes: 12_000,
        max_rounds: 4,
        max_duration_ms: 90_000,
        max_total_tokens: None,
    };
    let out = intersect_run_bounds(&[&server, &agent, &work]);
    assert_eq!(out.max_prompt_bytes, 8_000);
    assert_eq!(out.max_rounds, 4);
    assert_eq!(out.max_duration_ms, 30_000);
    assert_eq!(out.max_total_tokens, Some(20_000));
}

#[test]
fn abandoned_claiming_intent_returns_work_to_queued() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let intent = grokptah_agent_bridge::orchestration::ManagedExecutionIntent {
        schema_version: grokptah_agent_bridge::orchestration::MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id: "intent-1".into(),
        agent_id: "worker-a".into(),
        agent_spec_revision: 1,
        work_id: item.work_id.clone(),
        work_revision: claim.work.revision,
        attempt_id: Some(claim.attempt.attempt_id.clone()),
        run_id: None,
        session_id: session,
        workspace: "/tmp/ws".into(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        bounds: RunBounds::default(),
        execution_mode: RunExecutionMode::Shared,
        input_hash: "abc".into(),
        grok: None,
        state: ManagedIntentState::Claiming,
        permission_request_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.save_managed_intent(&intent).unwrap();
    store
        .abandon_managed_intent(&intent.intent_id, Utc::now())
        .unwrap();
    let restored = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(restored.state, WorkState::Queued);
}

#[test]
fn park_and_unknown_claim_do_not_create_a_second_live_run_link() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "run-1",
        )
        .unwrap();
    store
        .report_work_progress(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            WorkProgress {
                summary: "working".into(),
                percent: Some(10),
                updated_at: Utc::now(),
            },
        )
        .unwrap();
    let (parked, _) = store
        .park_work_input(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "permission required: shell",
        )
        .unwrap();
    assert_eq!(parked.state, WorkState::AwaitingInput);
    assert!(store.claim_work(&item.work_id, "other", None).is_err());
}

#[test]
fn two_store_opens_cannot_both_dispatch() {
    let home = tempdir().unwrap();
    let _first = OrchStore::open(home.path()).unwrap();
    assert!(OrchStore::open(home.path()).is_err());
}

#[test]
fn expired_attempt_rejects_late_managed_completion() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work(&item.work_id, "worker-a", Some(1))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let result = WorkResult {
        summary: "late".into(),
        evidence: Vec::new(),
        artifacts: Vec::new(),
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
        verification: None,
    };
    assert!(store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result,
        )
        .is_err());
}

#[test]
fn default_policy_is_disabled() {
    let policy = ManagedExecutionPolicy::default();
    assert!(!policy.enabled);
    assert_eq!(policy.native_execution_mode, RunExecutionMode::Shared);
}

#[test]
fn isolated_native_policy_is_explicit_approved_and_non_retrying() {
    let legacy_json = serde_json::to_value(ManagedExecutionPolicy::default()).unwrap();
    assert!(legacy_json.get("nativeExecutionMode").is_none());
    let legacy: ManagedExecutionPolicy = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(legacy.native_execution_mode, RunExecutionMode::Shared);

    let mut policy = ManagedExecutionPolicy {
        enabled: true,
        requires_approval_before_execution: true,
        native_execution_mode: RunExecutionMode::IsolatedWorktree,
        ..ManagedExecutionPolicy::default()
    };
    assert!(policy.validate().is_ok());

    policy.requires_approval_before_execution = false;
    assert_eq!(
        policy.validate().unwrap_err().code,
        OrchErrorCode::InvalidRequest
    );
    policy.requires_approval_before_execution = true;
    policy.retry_eligible = true;
    assert_eq!(
        policy.validate().unwrap_err().code,
        OrchErrorCode::InvalidRequest
    );
    policy.retry_eligible = false;
    policy.executor = ManagedExecutorKind::GrokBuildIsolatedReview;
    assert_eq!(
        policy.validate().unwrap_err().code,
        OrchErrorCode::InvalidRequest
    );
}

#[test]
fn isolated_native_work_requires_a_file_scope_and_current_authorization() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    store
        .revise_agent_spec("worker-a", "operator", |spec| {
            spec.managed_execution = ManagedExecutionPolicy {
                enabled: true,
                requires_approval_before_execution: true,
                native_execution_mode: RunExecutionMode::IsolatedWorktree,
                ..ManagedExecutionPolicy::default()
            };
            Ok(())
        })
        .unwrap();
    let worker = store.load_agent("worker-a").unwrap().unwrap();
    let spec = worker.current_spec().unwrap().clone();
    let ceiling = RunBounds::default();
    let unscoped = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&unscoped).unwrap();
    let (authorized_unscoped, unscoped_decision) = store
        .authorize_work_execution(
            &unscoped.work_id,
            "operator",
            None,
            "authorize one unscoped native attempt",
            Some(unscoped.revision),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        managed_execution_eligible(
            &authorized_unscoped,
            &worker,
            &spec,
            &[unscoped_decision],
            0,
            &ceiling,
        )
        .unwrap_err()
        .code,
        OrchErrorCode::ForbiddenScope
    );

    let mut scoped = accepted_work(session, "/tmp/ws", "worker-a");
    scoped.policy.allowed_files = vec!["README.md".into()];
    scoped.policy.retry.max_attempts = 1;
    scoped.policy.retry.retry_failed = false;
    scoped.policy.retry.retry_expired = false;
    store.save_work_item(&scoped).unwrap();
    let (authorized, decision) = store
        .authorize_work_execution(
            &scoped.work_id,
            "operator",
            None,
            "authorize one isolated native attempt",
            Some(scoped.revision),
            Utc::now(),
        )
        .unwrap();
    let eligible = managed_execution_eligible(
        &authorized,
        &worker,
        &spec,
        std::slice::from_ref(&decision),
        0,
        &ceiling,
    );
    assert!(eligible.is_ok(), "{eligible:?}");

    let mut missing_approval = spec.clone();
    missing_approval
        .managed_execution
        .requires_approval_before_execution = false;
    assert_eq!(
        managed_execution_eligible(
            &authorized,
            &worker,
            &missing_approval,
            std::slice::from_ref(&decision),
            0,
            &ceiling,
        )
        .unwrap_err()
        .code,
        OrchErrorCode::Conflict
    );
    let mut retrying_spec = spec.clone();
    retrying_spec.managed_execution.retry_eligible = true;
    assert_eq!(
        managed_execution_eligible(
            &authorized,
            &worker,
            &retrying_spec,
            std::slice::from_ref(&decision),
            0,
            &ceiling,
        )
        .unwrap_err()
        .code,
        OrchErrorCode::Conflict
    );
    let mut retrying_work = authorized;
    retrying_work.policy.retry.max_attempts = 2;
    retrying_work.policy.retry.retry_failed = true;
    assert_eq!(
        managed_execution_eligible(&retrying_work, &worker, &spec, &[decision], 0, &ceiling,)
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
}

fn enable_managed(store: &OrchStore, agent_id: &str, retry_eligible: bool) {
    store
        .revise_agent_spec(agent_id, "operator", |spec| {
            spec.managed_execution.enabled = true;
            spec.managed_execution.retry_eligible = retry_eligible;
            spec.managed_execution.bounds.max_total_tokens = Some(8_000);
            spec.authority.bypass_permissions = false;
            spec.authority.computer_use_allowed = false;
            Ok(())
        })
        .unwrap();
}

fn claiming_intent(
    work: &WorkItem,
    attempt_id: Option<String>,
    run_id: Option<String>,
    session: Uuid,
) -> ManagedExecutionIntent {
    let now = Utc::now();
    ManagedExecutionIntent {
        schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id: "intent-admit-1".into(),
        agent_id: "worker-a".into(),
        agent_spec_revision: 2,
        work_id: work.work_id.clone(),
        work_revision: work.revision,
        attempt_id,
        run_id,
        session_id: session,
        workspace: work.workspace.clone(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        bounds: RunBounds::default(),
        execution_mode: RunExecutionMode::Shared,
        input_hash: "hash".into(),
        grok: None,
        state: ManagedIntentState::Claiming,
        permission_request_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn run_for_intent(intent_id: &str, session: Uuid, workspace: &str, state: RunState) -> RunRecord {
    RunRecord {
        run_id: format!("run-{intent_id}"),
        session_id: session,
        workspace: workspace.into(),
        request_id: intent_id.into(),
        client_id: Some("native-executor".into()),
        state,
        purpose: Default::default(),
        agent_id: Some("worker-a".into()),
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: Some(2),
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "preview".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

#[test]
fn claiming_intent_without_claim_is_abandoned() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    store
        .save_managed_intent(&claiming_intent(&item, None, None, session))
        .unwrap();
    let recovered = store
        .reconcile_claiming_intent("test-owner", "intent-admit-1", "secret", Utc::now())
        .unwrap()
        .unwrap();
    assert_eq!(recovered.state, ManagedIntentState::Abandoned);
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Queued
    );
}

#[test]
fn claiming_intent_after_claim_without_run_releases_attempt() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    store
        .save_managed_intent(&claiming_intent(
            &item,
            Some(claim.attempt.attempt_id.clone()),
            None,
            session,
        ))
        .unwrap();
    store
        .reconcile_claiming_intent("test-owner", "intent-admit-1", "secret", Utc::now())
        .unwrap();
    let restored = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(restored.state, WorkState::Queued);
    let attempts = store.list_work_attempts(Some(&item.work_id)).unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(!attempts[0].state.is_active());
}

#[test]
fn dispatching_intent_cannot_be_abandoned_or_requeued_after_possible_send() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let work = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&work).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&work.work_id, "worker-a", None, "secret")
        .unwrap();
    let mut intent = claiming_intent(
        &claim.work,
        Some(claim.attempt.attempt_id.clone()),
        None,
        session,
    );
    intent.state = ManagedIntentState::Dispatching;
    store.save_managed_intent(&intent).unwrap();

    let unchanged = store
        .abandon_managed_intent(&intent.intent_id, Utc::now())
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.state, ManagedIntentState::Dispatching);
    assert_ne!(
        store.load_work_item(&work.work_id).unwrap().unwrap().state,
        WorkState::Queued
    );

    let reviewed = store
        .finalize_managed_intent(
            &intent.intent_id,
            grokptah_agent_bridge::orchestration::ManagedFinalizationOutcome::Review,
            "dispatch outcome is uncertain after restart",
            Some(WorkResult {
                summary: "dispatch outcome is uncertain after restart".into(),
                evidence: vec!["executor:grok_build_isolated_review".into()],
                artifacts: Vec::new(),
                failure: Some("grok_dispatch_uncertain_after_restart".into()),
                cancellation_reason: None,
                completed_at: Utc::now(),
                verification: None,
            }),
            Utc::now(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(reviewed.state, ManagedIntentState::Finalized);
    let work = store.load_work_item(&work.work_id).unwrap().unwrap();
    assert_eq!(work.state, WorkState::Review);
    assert!(work.result.unwrap().verification.is_none());
}

#[test]
fn claiming_intent_adopts_already_committed_run() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let run = run_for_intent("intent-admit-1", session, "/tmp/ws", RunState::Running);
    store.save_run(&run).unwrap();
    store
        .save_managed_intent(&claiming_intent(
            &item,
            Some(claim.attempt.attempt_id.clone()),
            None,
            session,
        ))
        .unwrap();
    let recovered = store
        .reconcile_claiming_intent("test-owner", "intent-admit-1", "secret", Utc::now())
        .unwrap()
        .unwrap();
    assert_eq!(recovered.state, ManagedIntentState::Admitted);
    assert_eq!(recovered.run_id.as_deref(), Some(run.run_id.as_str()));
    let attempt = store
        .load_work_attempt(&claim.attempt.attempt_id)
        .unwrap()
        .unwrap();
    assert_eq!(attempt.linked_run_ids, vec![run.run_id.clone()]);
    assert_eq!(store.live_managed_intents_for_agent("worker-a").unwrap(), 1);
}

#[test]
fn linked_attempt_before_intent_commit_recovers_one_run() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let run = run_for_intent("intent-admit-1", session, "/tmp/ws", RunState::Running);
    store.save_run(&run).unwrap();
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run.run_id,
        )
        .unwrap();
    store
        .save_managed_intent(&claiming_intent(
            &item,
            Some(claim.attempt.attempt_id.clone()),
            None,
            session,
        ))
        .unwrap();
    let recovered = store
        .reconcile_claiming_intent("test-owner", "intent-admit-1", "secret", Utc::now())
        .unwrap()
        .unwrap();
    assert_eq!(recovered.state, ManagedIntentState::Admitted);
    let attempt = store
        .load_work_attempt(&claim.attempt.attempt_id)
        .unwrap()
        .unwrap();
    assert_eq!(attempt.linked_run_ids.len(), 1);
}

#[test]
fn interrupted_run_with_retry_forbidden_is_terminal() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    enable_managed(&store, "worker-a", false);
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let mut intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-int".into()),
        session,
    );
    intent.state = ManagedIntentState::Admitted;
    store.save_managed_intent(&intent).unwrap();
    let closed = store
        .close_managed_attempt(
            &intent.intent_id,
            false,
            ManagedRetryCause::Interrupted,
            "interrupted",
            Utc::now(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(closed.state, ManagedIntentState::Finalized);
    let work = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(work.state, WorkState::Failed);
    assert!(store.live_managed_intents_for_agent("worker-a").unwrap() == 0);
}

#[test]
fn interrupted_run_with_retry_allowed_requeues_without_resuming() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    enable_managed(&store, "worker-a", true);
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let mut intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-int".into()),
        session,
    );
    intent.state = ManagedIntentState::Admitted;
    store.save_managed_intent(&intent).unwrap();
    store
        .close_managed_attempt(
            &intent.intent_id,
            true,
            ManagedRetryCause::Interrupted,
            "interrupted",
            Utc::now(),
        )
        .unwrap();
    let work = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(work.state, WorkState::Queued);
    let attempt = store
        .load_work_attempt(&claim.attempt.attempt_id)
        .unwrap()
        .unwrap();
    assert!(!attempt.state.is_active());
    let spec = store
        .load_agent("worker-a")
        .unwrap()
        .unwrap()
        .current_spec()
        .unwrap()
        .clone();
    assert!(spec.managed_execution.allows_auto_retry(
        &work,
        work.attempt_count + 1,
        ManagedRetryCause::Interrupted
    ));
}

#[test]
fn managed_close_preserves_review_gate_even_when_retry_is_requested() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    enable_managed(&store, "worker-a", true);
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let (review, review_attempt) = store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            WorkResult {
                summary: "advisory requires review".into(),
                evidence: Vec::new(),
                artifacts: Vec::new(),
                failure: None,
                cancellation_reason: None,
                completed_at: Utc::now(),
                verification: None,
            },
        )
        .unwrap();
    assert_eq!(review.state, WorkState::Review);
    assert_eq!(review_attempt.state, AttemptState::Review);

    let mut expiring = review.clone();
    expiring.deadline = Some(Utc::now() - chrono::Duration::seconds(1));
    store.save_work_item(&expiring).unwrap();
    let mut intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-review".into()),
        session,
    );
    intent.state = ManagedIntentState::Admitted;
    store.save_managed_intent(&intent).unwrap();

    let original_result = review_attempt.result.clone();
    store
        .finalize_managed_intent_until(
            &intent.intent_id,
            ManagedFinalizationOutcome::Failed,
            "stale failure record",
            Some(WorkResult {
                summary: "stale replacement".into(),
                evidence: Vec::new(),
                artifacts: Vec::new(),
                failure: Some("stale".into()),
                cancellation_reason: None,
                completed_at: Utc::now(),
                verification: None,
            }),
            Utc::now(),
            ManagedFinalizationStage::Complete,
        )
        .unwrap();
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Review
    );
    assert_eq!(
        store
            .load_work_attempt(&claim.attempt.attempt_id)
            .unwrap()
            .unwrap()
            .result,
        original_result
    );

    let mut close_intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-review-close".into()),
        session,
    );
    close_intent.intent_id = "intent-review-close".into();
    close_intent.state = ManagedIntentState::Admitted;
    store.save_managed_intent(&close_intent).unwrap();

    store
        .close_managed_attempt(
            &close_intent.intent_id,
            true,
            ManagedRetryCause::Interrupted,
            "child stopped after advisory result",
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Review
    );
    assert_eq!(
        store
            .load_work_attempt(&claim.attempt.attempt_id)
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Review
    );
}

#[test]
fn resolve_permission_requires_parked_scope_and_updates_attempt() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let other = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    store
        .park_work_input(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "permission required",
        )
        .unwrap();
    let mut intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-p".into()),
        session,
    );
    intent.state = ManagedIntentState::Parked;
    intent.permission_request_id = Some("perm-1".into());
    store.save_managed_intent(&intent).unwrap();
    assert_eq!(
        store
            .resolve_parked_managed_permission("perm-1", other, "/tmp/ws", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );
    assert_eq!(
        store
            .resolve_parked_managed_permission("perm-1", session, "/tmp/other", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );
    assert_eq!(
        store
            .resolve_parked_managed_permission("missing", session, "/tmp/ws", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::InvalidRequest
    );
    let parked = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(parked.state, WorkState::AwaitingInput);
    store
        .resolve_parked_managed_permission("perm-1", session, "/tmp/ws", Utc::now())
        .unwrap();
    let running = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(running.state, WorkState::Running);
    let attempt = store
        .load_work_attempt(&claim.attempt.attempt_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        attempt.state,
        grokptah_agent_bridge::orchestration::AttemptState::Running
    );
    assert_eq!(
        store
            .resolve_parked_managed_permission("perm-1", session, "/tmp/ws", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
    store
        .cancel_work(&item.work_id, "cancelled while parked")
        .unwrap();
    let mut again = intent.clone();
    again.state = ManagedIntentState::Parked;
    again.permission_request_id = Some("perm-2".into());
    store.save_managed_intent(&again).unwrap();
    assert_eq!(
        store
            .resolve_parked_managed_permission("perm-2", session, "/tmp/ws", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Cancelled
    );
}

#[test]
fn retry_eligible_false_blocks_second_native_admission() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    enable_managed(&store, "worker-a", false);
    let mut item = accepted_work(session, "/tmp/ws", "worker-a");
    item.attempt_count = 1;
    store.save_work_item(&item).unwrap();
    let agent = store.load_agent("worker-a").unwrap().unwrap();
    let spec = agent.current_spec().unwrap().clone();
    let err = managed_execution_eligible(&item, &agent, &spec, &[], 0, &RunBounds::default())
        .unwrap_err();
    assert_eq!(err.code, OrchErrorCode::Conflict);
}

#[test]
fn assemble_uses_work_and_server_limits_smaller_than_agent() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    enable_managed(&store, "worker-a", false);
    let agent = store.load_agent("worker-a").unwrap().unwrap();
    let spec = agent.current_spec().unwrap().clone();
    let work = accepted_work(session, "/tmp/ws", "worker-a");
    let server = RunBounds {
        max_prompt_bytes: 80,
        max_rounds: 2,
        max_duration_ms: 1_000,
        max_total_tokens: Some(100),
    };
    let work_bounds = RunBounds {
        max_prompt_bytes: 70,
        max_rounds: 8,
        max_duration_ms: 5_000,
        max_total_tokens: Some(200),
    };
    let effective = intersect_run_bounds(&[
        &server,
        &spec.default_run_bounds,
        &spec.managed_execution.bounds,
        &work_bounds,
    ]);
    let (body, _) =
        assemble_managed_run_input(&work, &spec, &effective, 1, None, &[], None).unwrap();
    assert!(body.len() <= effective.max_prompt_bytes);
    assert!(effective.max_prompt_bytes <= 70);
}

#[test]
fn relevant_context_from_busy_lane_is_work_and_agent_scoped() {
    let session = Uuid::new_v4();
    let now = Utc::now();
    let work = accepted_work(session, "/tmp/ws", "worker-a");
    let mut messages = Vec::new();
    for seq in 1..=30u64 {
        let mut message = WorkMessage::new(
            MessageKind::Status,
            "actor",
            Some("other".into()),
            Some("other".into()),
            session,
            "/tmp/ws",
            Some("unrelated".into()),
            format!("noise-{seq}"),
            None,
            now,
        )
        .unwrap();
        message.seq = seq;
        messages.push(message);
    }
    let mut instruction = WorkMessage::new(
        MessageKind::Instruction,
        "coord",
        Some("manager".into()),
        Some("worker-a".into()),
        session,
        "/tmp/ws",
        Some(work.work_id.clone()),
        "use the current fixture",
        None,
        now,
    )
    .unwrap();
    instruction.seq = 31;
    messages.push(instruction);
    let selected = select_relevant_managed_messages(&messages, &work, "worker-a", now, 16);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].body, "use the current fixture");
}

#[test]
fn failed_and_expired_causes_obey_retry_eligible() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    enable_managed(&store, "worker-a", false);
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let mut intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-fail".into()),
        session,
    );
    intent.state = ManagedIntentState::Admitted;
    store.save_managed_intent(&intent).unwrap();
    store
        .close_managed_attempt(
            &intent.intent_id,
            false,
            ManagedRetryCause::Failed,
            "failed",
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Failed
    );
}

#[test]
fn resolve_stale_non_parked_and_denial_do_not_mutate_wrongly() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let mut admitted = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-p".into()),
        session,
    );
    admitted.state = ManagedIntentState::Admitted;
    admitted.permission_request_id = Some("perm-stale".into());
    store.save_managed_intent(&admitted).unwrap();
    assert_eq!(
        store
            .resolve_parked_managed_permission("perm-stale", session, "/tmp/ws", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
    let still_leased = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_ne!(still_leased.state, WorkState::Running);
    store
        .park_work_input(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "permission required",
        )
        .unwrap();
    let mut parked = admitted;
    parked.intent_id = "intent-stale".into();
    parked.state = ManagedIntentState::Parked;
    parked.permission_request_id = Some("perm-wait".into());
    store.save_managed_intent(&parked).unwrap();
    let mut running = store.load_work_item(&item.work_id).unwrap().unwrap();
    running.state = WorkState::Running;
    store.save_work_item(&running).unwrap();
    assert_eq!(
        store
            .resolve_parked_managed_permission("perm-wait", session, "/tmp/ws", Utc::now())
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Running
    );
}

#[test]
fn relevant_context_keeps_newest_work_and_agent_messages() {
    let session = Uuid::new_v4();
    let now = Utc::now();
    let work = accepted_work(session, "/tmp/ws", "worker-a");
    let mut messages = Vec::new();
    for seq in 1..=20u64 {
        let mut message = WorkMessage::new(
            MessageKind::Instruction,
            "coord",
            Some("manager".into()),
            Some("worker-a".into()),
            session,
            "/tmp/ws",
            Some(work.work_id.clone()),
            format!("work-instruction-{seq}"),
            None,
            now,
        )
        .unwrap();
        message.seq = seq;
        messages.push(message);
    }
    for seq in 21..=30u64 {
        let mut message = WorkMessage::new(
            MessageKind::Status,
            "actor",
            Some("other-agent".into()),
            Some("other-agent".into()),
            session,
            "/tmp/ws",
            Some("unrelated".into()),
            format!("other-work-{seq}"),
            None,
            now,
        )
        .unwrap();
        message.seq = seq;
        messages.push(message);
    }
    let mut expired = WorkMessage::new(
        MessageKind::Question,
        "coord",
        Some("manager".into()),
        Some("worker-a".into()),
        session,
        "/tmp/ws",
        Some(work.work_id.clone()),
        "expired question",
        None,
        now,
    )
    .unwrap();
    expired.seq = 31;
    expired.expires_at = Some(now - chrono::Duration::minutes(1));
    messages.push(expired);
    let mut dup_a = WorkMessage::new(
        MessageKind::Status,
        "worker-a",
        Some("worker-a".into()),
        Some("manager".into()),
        session,
        "/tmp/ws",
        Some(work.work_id.clone()),
        "same thread body",
        None,
        now,
    )
    .unwrap();
    dup_a.seq = 32;
    dup_a.thread_id = Some("thread-1".into());
    messages.push(dup_a);
    let mut dup_b = WorkMessage::new(
        MessageKind::Status,
        "worker-a",
        Some("worker-a".into()),
        Some("manager".into()),
        session,
        "/tmp/ws",
        Some(work.work_id.clone()),
        "same thread body",
        None,
        now,
    )
    .unwrap();
    dup_b.seq = 33;
    dup_b.thread_id = Some("thread-1".into());
    messages.push(dup_b);
    let mut current = WorkMessage::new(
        MessageKind::Instruction,
        "coord",
        Some("manager".into()),
        Some("worker-a".into()),
        session,
        "/tmp/ws",
        Some(work.work_id.clone()),
        "current work-specific instructions",
        None,
        now,
    )
    .unwrap();
    current.seq = 34;
    messages.push(current);
    let selected = select_relevant_managed_messages(&messages, &work, "worker-a", now, 16);
    assert_eq!(selected.len(), 16);
    assert!(!selected
        .iter()
        .any(|message| message.body.starts_with("other-work-")));
    assert!(!selected
        .iter()
        .any(|message| message.body == "expired question"));
    assert!(selected
        .iter()
        .any(|message| message.body == "current work-specific instructions"));
    assert_eq!(
        selected
            .iter()
            .filter(|message| message.body == "same thread body")
            .count(),
        1
    );
    assert_eq!(selected.last().unwrap().seq, 34);
    assert!(selected.first().unwrap().seq > 1);
}

#[test]
fn list_recent_messages_reads_the_newest_retained_window() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let work = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&work).unwrap();
    let now = Utc::now();
    for seq in 1..=500u64 {
        let work_id = if seq > 400 {
            Some(work.work_id.clone())
        } else if seq % 17 == 0 {
            Some("unrelated".into())
        } else {
            None
        };
        let kind = if seq == 450 {
            MessageKind::Question
        } else {
            MessageKind::Instruction
        };
        let mut message = WorkMessage::new(
            kind,
            "actor",
            None,
            Some("worker-a".into()),
            session,
            "/tmp/ws",
            work_id,
            if seq > 400 {
                format!("late-instruction-{seq}")
            } else {
                format!("old-{seq}")
            },
            None,
            now,
        )
        .unwrap();
        if seq == 450 {
            message.expires_at = Some(now - chrono::Duration::minutes(1));
        }
        if seq == 480 || seq == 481 {
            message.thread_id = Some("dup".into());
            message.body = "duplicate thread".into();
        }
        store.send_message(message).unwrap();
    }
    let oldest = store
        .list_messages(session, "/tmp/ws", 0, None, None, 200)
        .unwrap();
    assert!(oldest.messages.last().unwrap().seq <= 200);
    let newest = store
        .list_recent_messages(session, "/tmp/ws", None, None, 200)
        .unwrap();
    assert!(newest.messages.first().unwrap().seq >= 300);
    let selected = select_relevant_managed_messages(&newest.messages, &work, "worker-a", now, 16);
    assert!(selected
        .iter()
        .all(|message| message.seq > 400 || message.from_agent_id.is_none()));
    assert!(selected.iter().any(|message| message.seq > 400));
    assert!(!selected
        .iter()
        .any(|message| message.body.starts_with("old-")));
    assert!(!selected
        .iter()
        .any(|message| message.body == "late-instruction-450"));
}

fn crash_close_stage(stage: ManagedFinalizationStage) {
    let home = tempdir().unwrap();
    let path = home.path().to_path_buf();
    let session = Uuid::new_v4();
    let (work_id, intent_id, attempt_id) = {
        let store = OrchStore::open(&path).unwrap();
        store
            .save_agent(&agent("worker-a", "/tmp/ws", session))
            .unwrap();
        enable_managed(&store, "worker-a", true);
        let item = accepted_work(session, "/tmp/ws", "worker-a");
        store.save_work_item(&item).unwrap();
        let claim = store
            .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
            .unwrap();
        let mut intent = claiming_intent(
            &item,
            Some(claim.attempt.attempt_id.clone()),
            Some("run-int".into()),
            session,
        );
        intent.state = ManagedIntentState::Admitted;
        store.save_managed_intent(&intent).unwrap();
        store
            .close_managed_attempt_until(
                &intent.intent_id,
                false,
                ManagedRetryCause::Interrupted,
                "interrupted",
                Utc::now(),
                stage,
            )
            .unwrap();
        let loaded = store
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap();
        match stage {
            ManagedFinalizationStage::BeforeJournal => {
                assert_eq!(loaded.state, ManagedIntentState::Admitted);
                assert!(store
                    .load_work_attempt(&claim.attempt.attempt_id)
                    .unwrap()
                    .unwrap()
                    .state
                    .is_active());
                assert!(
                    !store
                        .load_work_item(&item.work_id)
                        .unwrap()
                        .unwrap()
                        .state
                        .is_terminal(),
                    "work must remain non-terminal before the journal lands"
                );
            }
            ManagedFinalizationStage::AfterJournal => {
                assert_eq!(loaded.state, ManagedIntentState::Admitted);
                assert!(store
                    .load_work_attempt(&claim.attempt.attempt_id)
                    .unwrap()
                    .unwrap()
                    .state
                    .is_active());
            }
            ManagedFinalizationStage::AfterAttempt => {
                assert!(loaded.state.is_live());
                assert!(!store
                    .load_work_attempt(&claim.attempt.attempt_id)
                    .unwrap()
                    .unwrap()
                    .state
                    .is_active());
            }
            ManagedFinalizationStage::AfterWork => {
                assert!(loaded.state.is_live());
                assert_eq!(
                    store.load_work_item(&item.work_id).unwrap().unwrap().state,
                    WorkState::Failed
                );
            }
            ManagedFinalizationStage::Complete => {
                assert_eq!(loaded.state, ManagedIntentState::Finalized);
            }
        }
        (item.work_id, intent.intent_id, claim.attempt.attempt_id)
    };
    let store = OrchStore::open(&path).unwrap();
    if stage == ManagedFinalizationStage::BeforeJournal {
        let intent = store.load_managed_intent(&intent_id).unwrap().unwrap();
        assert_eq!(intent.state, ManagedIntentState::Admitted);
        assert!(
            !store
                .load_work_item(&work_id)
                .unwrap()
                .unwrap()
                .state
                .is_terminal(),
            "work must remain non-terminal when the journal never landed"
        );
        assert!(store
            .load_work_attempt(&attempt_id)
            .unwrap()
            .unwrap()
            .state
            .is_active());
        store
            .close_managed_attempt(
                &intent_id,
                false,
                ManagedRetryCause::Interrupted,
                "interrupted",
                Utc::now(),
            )
            .unwrap();
    }
    let intent = store.load_managed_intent(&intent_id).unwrap().unwrap();
    assert_eq!(intent.state, ManagedIntentState::Finalized);
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().state,
        WorkState::Failed
    );
    assert!(!store
        .load_work_attempt(&attempt_id)
        .unwrap()
        .unwrap()
        .state
        .is_active());
    assert_eq!(store.live_managed_intents_for_agent("worker-a").unwrap(), 0);
}

#[test]
fn managed_finalization_converges_before_journal_crash() {
    crash_close_stage(ManagedFinalizationStage::BeforeJournal);
}

#[test]
fn managed_finalization_converges_after_journal_only_crash() {
    crash_close_stage(ManagedFinalizationStage::AfterJournal);
}

#[test]
fn managed_finalization_converges_after_attempt_write_crash() {
    crash_close_stage(ManagedFinalizationStage::AfterAttempt);
}

#[test]
fn managed_finalization_converges_after_work_write_crash() {
    crash_close_stage(ManagedFinalizationStage::AfterWork);
}

#[test]
fn managed_finalization_complete_is_idempotent() {
    crash_close_stage(ManagedFinalizationStage::Complete);
}

#[test]
fn managed_finalization_preserves_cancelled_work() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let mut intent = claiming_intent(
        &item,
        Some(claim.attempt.attempt_id.clone()),
        Some("run-c".into()),
        session,
    );
    intent.state = ManagedIntentState::Admitted;
    store.save_managed_intent(&intent).unwrap();
    store
        .cancel_work(&item.work_id, "operator cancelled")
        .unwrap();
    store
        .close_managed_attempt(
            &intent.intent_id,
            true,
            ManagedRetryCause::Interrupted,
            "interrupted",
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Cancelled
    );
    assert_eq!(
        store
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Finalized
    );
}

#[test]
fn completed_finalization_converges_after_partial_writes() {
    let home = tempdir().unwrap();
    let path = home.path().to_path_buf();
    let session = Uuid::new_v4();
    let (work_id, intent_id) = {
        let store = OrchStore::open(&path).unwrap();
        store
            .save_agent(&agent("worker-a", "/tmp/ws", session))
            .unwrap();
        let item = accepted_work(session, "/tmp/ws", "worker-a");
        store.save_work_item(&item).unwrap();
        let claim = store
            .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
            .unwrap();
        let mut intent = claiming_intent(
            &item,
            Some(claim.attempt.attempt_id.clone()),
            Some("run-ok".into()),
            session,
        );
        intent.state = ManagedIntentState::Admitted;
        store.save_managed_intent(&intent).unwrap();
        let result = WorkResult {
            summary: "done".into(),
            evidence: Vec::new(),
            artifacts: Vec::new(),
            failure: None,
            cancellation_reason: None,
            completed_at: Utc::now(),
            verification: None,
        };
        store
            .finalize_managed_intent_until(
                &intent.intent_id,
                grokptah_agent_bridge::orchestration::ManagedFinalizationOutcome::Completed,
                "completed",
                Some(result),
                Utc::now(),
                ManagedFinalizationStage::AfterAttempt,
            )
            .unwrap();
        assert!(store
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap()
            .state
            .is_live());
        (item.work_id, intent.intent_id)
    };
    let store = OrchStore::open(&path).unwrap();
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().state,
        WorkState::Review
    );
    assert_eq!(
        store
            .load_managed_intent(&intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Finalized
    );
}

#[test]
fn seal_queued_managed_work_converges_after_store_reload() {
    let home = tempdir().unwrap();
    let work_id;
    {
        let store = OrchStore::open(home.path()).unwrap();
        let session = Uuid::new_v4();
        let item = accepted_work(session, "/tmp/ws", "worker-a");
        store.save_work_item(&item).unwrap();
        work_id = item.work_id.clone();
        store
            .seal_queued_managed_work(&work_id, "lane sealed", Utc::now())
            .unwrap();
        assert_eq!(
            store.load_work_item(&work_id).unwrap().unwrap().state,
            WorkState::Failed
        );
    }
    let store = OrchStore::open(home.path()).unwrap();
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().state,
        WorkState::Failed
    );
}
