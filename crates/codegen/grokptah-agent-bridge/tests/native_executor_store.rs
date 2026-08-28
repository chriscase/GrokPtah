use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    assemble_managed_run_input, intersect_run_bounds, managed_execution_eligible,
    select_relevant_managed_messages, AssignmentStatus, ManagedExecutionIntent,
    ManagedExecutionPolicy, ManagedFinalizationStage, ManagedIntentState, ManagedRetryCause,
    ManagedWorkMode, MessageKind, OrchErrorCode, OrchStore, RunBounds, RunRecord, RunState,
    WorkItem, WorkMessage, WorkPolicy, WorkProgress, WorkResult, WorkState,
    MANAGED_EXECUTION_SCHEMA_VERSION,
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
        input_hash: "abc".into(),
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
    assert!(!ManagedExecutionPolicy::default().enabled);
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
        input_hash: "hash".into(),
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
        client_id: Some("actor_native_fixture".into()),
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
        .reconcile_claiming_intent("intent-admit-1", "secret", Utc::now())
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
        .reconcile_claiming_intent("intent-admit-1", "secret", Utc::now())
        .unwrap();
    let restored = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(restored.state, WorkState::Queued);
    let attempts = store.list_work_attempts(Some(&item.work_id)).unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(!attempts[0].state.is_active());
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
        .reconcile_claiming_intent("intent-admit-1", "secret", Utc::now())
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
        .reconcile_claiming_intent("intent-admit-1", "secret", Utc::now())
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
        WorkState::Succeeded
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
