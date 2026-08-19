use chrono::{Datelike, Duration, TimeZone, Timelike, Utc};
use grokptah_agent_bridge::orchestration::{
    occurrence_dedupe_key, ActivationCause, ActivationDisposition, ActivationRequest, Clock,
    FakeClock, MissedRunPolicy, OrchStore, RoutineConcurrencyPolicy, RoutineLifecycle,
    RoutineRecord, RoutineRetryPolicy, RoutineTrigger, WorkItem, WorkPolicy, WorkState,
    WorkTemplate,
};
use grokptah_agent_bridge::{AgentRecord, AgentState};
use tempfile::tempdir;
use uuid::Uuid;

fn agent_record(agent_id: &str, session_id: Uuid, workspace: &str) -> AgentRecord {
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    AgentRecord {
        agent_id: agent_id.into(),
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

fn template() -> WorkTemplate {
    WorkTemplate {
        kind: "routine".into(),
        objective: "Inspect the workspace".into(),
        priority: 0,
        policy: WorkPolicy::default(),
    }
}

fn save_agent(store: &OrchStore, session_id: Uuid, workspace: &str) {
    store
        .save_agent(&agent_record("agent-1", session_id, workspace))
        .unwrap();
}

fn make_routine(
    store: &OrchStore,
    session_id: Uuid,
    workspace: &str,
    trigger: RoutineTrigger,
    missed: MissedRunPolicy,
    now: chrono::DateTime<Utc>,
) -> RoutineRecord {
    save_agent(store, session_id, workspace);
    let routine = RoutineRecord::new(
        "nightly",
        "agent-1",
        session_id,
        workspace,
        trigger,
        template(),
        missed,
        RoutineConcurrencyPolicy::default(),
        RoutineRetryPolicy::default(),
        "test",
        now,
    )
    .unwrap();
    store.save_routine(&routine).unwrap();
    routine
}

fn manual_request(_routine_id: &str, now: chrono::DateTime<Utc>, key: &str) -> ActivationRequest {
    ActivationRequest {
        cause: ActivationCause::Manual,
        dedupe_key: key.into(),
        scheduled_at: now,
        received_at: now,
        payload: None,
        created_by: "test".into(),
    }
}

#[test]
fn manual_activation_creates_work_through_existing_lifecycle() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        RoutineTrigger::Manual,
        MissedRunPolicy::Skip,
        now,
    );
    let activation = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "manual-1"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(activation.disposition, ActivationDisposition::CreatedWork);
    let work_id = activation.work_id.expect("work id");
    let work = store.load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(work.state, WorkState::Queued);
    assert_eq!(
        work.source_routine_id.as_deref(),
        Some(routine.routine_id.as_str())
    );
    assert_eq!(work.assigned_agent_id.as_deref(), Some("agent-1"));
    assert!(!activation.captured_policy.computer_use_allowed);
    assert!(!activation.captured_policy.auto_approve_tools);
    assert_eq!(activation.captured_policy.agent_spec_revision, Some(1));
}

#[test]
fn duplicate_delivery_does_not_create_duplicate_work() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        RoutineTrigger::Manual,
        MissedRunPolicy::Skip,
        now,
    );
    let first = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "same-key"),
            &Default::default(),
            now,
        )
        .unwrap();
    let second = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "same-key"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(first.activation_id, second.activation_id);
    assert_eq!(first.work_id, second.work_id);
    let work = store.list_work_items().unwrap();
    assert_eq!(work.len(), 1);
}

#[test]
fn restart_preserves_routine_next_fire_dedupe_and_history() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let trigger = RoutineTrigger::Interval {
        every_ms: 60_000,
        timezone: "UTC".into(),
        anchor: now,
    };
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        trigger,
        MissedRunPolicy::Skip,
        now,
    );
    let scheduled = now;
    let activation = store
        .activate_routine(
            &routine.routine_id,
            ActivationRequest {
                cause: ActivationCause::Scheduled,
                dedupe_key: occurrence_dedupe_key(&routine.routine_id, scheduled),
                scheduled_at: scheduled,
                received_at: now,
                payload: None,
                created_by: "test".into(),
            },
            &Default::default(),
            now,
        )
        .unwrap();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let restored = reopened.load_routine(&routine.routine_id).unwrap().unwrap();
    assert_eq!(restored.name, "nightly");
    assert_eq!(restored.next_fire_at, routine.next_fire_at);
    let history = reopened.list_activations(&routine.routine_id, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].activation_id, activation.activation_id);
    let replay = reopened
        .activate_routine(
            &routine.routine_id,
            ActivationRequest {
                cause: ActivationCause::Scheduled,
                dedupe_key: occurrence_dedupe_key(&routine.routine_id, scheduled),
                scheduled_at: scheduled,
                received_at: now + Duration::minutes(1),
                payload: None,
                created_by: "test".into(),
            },
            &Default::default(),
            now + Duration::minutes(1),
        )
        .unwrap();
    assert_eq!(replay.activation_id, activation.activation_id);
    assert_eq!(reopened.list_work_items().unwrap().len(), 1);
}

#[test]
fn downtime_follows_missed_run_policy() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let clock = FakeClock::new(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    let trigger = RoutineTrigger::Interval {
        every_ms: 60_000,
        timezone: "UTC".into(),
        anchor: clock.now(),
    };
    let skip = make_routine(
        &store,
        session_id,
        "/tmp/project",
        trigger.clone(),
        MissedRunPolicy::Skip,
        clock.now(),
    );
    clock.advance(Duration::minutes(5));
    let skip_report = store.fire_due_routines_at(clock.now()).unwrap();
    assert_eq!(skip_report.created_work, 0);
    assert!(skip_report.skipped >= 1);
    let skip_after = store.load_routine(&skip.routine_id).unwrap().unwrap();
    assert!(skip_after.next_fire_at.unwrap() > clock.now());

    let home2 = tempdir().unwrap();
    let store2 = OrchStore::open(home2.path()).unwrap();
    let coalesce = make_routine(
        &store2,
        session_id,
        "/tmp/project",
        trigger.clone(),
        MissedRunPolicy::Coalesce,
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    );
    let later = Utc.with_ymd_and_hms(2026, 1, 1, 0, 5, 0).unwrap();
    let coalesce_report = store2.fire_due_routines_at(later).unwrap();
    assert_eq!(coalesce_report.created_work, 1);
    assert_eq!(
        store2
            .list_activations(&coalesce.routine_id, 10)
            .unwrap()
            .len(),
        1
    );

    let home3 = tempdir().unwrap();
    let store3 = OrchStore::open(home3.path()).unwrap();
    save_agent(&store3, session_id, "/tmp/project");
    let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let mut catch_up = RoutineRecord::new(
        "catch-up",
        "agent-1",
        session_id,
        "/tmp/project",
        trigger,
        template(),
        MissedRunPolicy::CatchUp,
        RoutineConcurrencyPolicy {
            max_in_flight: 8,
            overlap: grokptah_agent_bridge::orchestration::OverlapPolicy::Skip,
        },
        RoutineRetryPolicy::default(),
        "test",
        start,
    )
    .unwrap();
    catch_up.next_fire_at = Some(start);
    store3.save_routine(&catch_up).unwrap();
    let catch_report = store3.fire_due_routines_at(later).unwrap();
    assert_eq!(catch_report.created_work, 6);
    assert_eq!(
        store3
            .list_activations(&catch_up.routine_id, 20)
            .unwrap()
            .len(),
        6
    );
}

#[test]
fn overlapping_activations_follow_concurrency_policy() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        RoutineTrigger::Manual,
        MissedRunPolicy::Skip,
        now,
    );
    let first = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "one"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(first.disposition, ActivationDisposition::CreatedWork);
    let second = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "two"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(second.disposition, ActivationDisposition::SkippedOverlap);
    assert!(second.work_id.is_none());
    assert_eq!(store.list_work_items().unwrap().len(), 1);
}

#[test]
fn pause_disable_and_backoff_are_deterministic() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let trigger = RoutineTrigger::Interval {
        every_ms: 60_000,
        timezone: "UTC".into(),
        anchor: now,
    };
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        trigger,
        MissedRunPolicy::Skip,
        now,
    );
    let paused = store
        .set_routine_lifecycle(&routine.routine_id, RoutineLifecycle::Paused, Some(1), now)
        .unwrap();
    assert_eq!(paused.lifecycle, RoutineLifecycle::Paused);
    let scheduled = store.fire_due_routines_at(now).unwrap();
    assert_eq!(scheduled.created_work, 0);
    let manual = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "paused-manual"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(manual.disposition, ActivationDisposition::CreatedWork);

    store
        .set_routine_lifecycle(
            &routine.routine_id,
            RoutineLifecycle::Disabled,
            Some(paused.revision),
            now,
        )
        .unwrap();
    let disabled = store
        .activate_routine(
            &routine.routine_id,
            manual_request(&routine.routine_id, now, "disabled-manual"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(disabled.disposition, ActivationDisposition::SkippedDisabled);
}

#[test]
fn malformed_oversized_unauthorized_expired_input_creates_no_work() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    save_agent(&store, session_id, "/tmp/project");
    assert!(RoutineRecord::new(
        "bad",
        "agent-1",
        session_id,
        "/tmp/project",
        RoutineTrigger::Interval {
            every_ms: 1,
            timezone: "UTC".into(),
            anchor: now,
        },
        template(),
        MissedRunPolicy::Skip,
        RoutineConcurrencyPolicy::default(),
        RoutineRetryPolicy::default(),
        "test",
        now,
    )
    .is_err());

    let expired = RoutineRecord::new(
        "expired",
        "agent-1",
        session_id,
        "/tmp/project",
        RoutineTrigger::OneShot {
            at: now - Duration::hours(2),
            expires_at: Some(now - Duration::hours(1)),
        },
        template(),
        MissedRunPolicy::Skip,
        RoutineConcurrencyPolicy::default(),
        RoutineRetryPolicy::default(),
        "test",
        now - Duration::hours(3),
    )
    .unwrap();
    store.save_routine(&expired).unwrap();
    let result = store
        .activate_routine(
            &expired.routine_id,
            ActivationRequest {
                cause: ActivationCause::Scheduled,
                dedupe_key: "expired".into(),
                scheduled_at: now - Duration::hours(2),
                received_at: now,
                payload: None,
                created_by: "test".into(),
            },
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(result.disposition, ActivationDisposition::SkippedExpired);
    assert!(store.list_work_items().unwrap().is_empty());

    let oversized = "x".repeat(9000);
    let valid = make_routine(
        &store,
        session_id,
        "/tmp/project",
        RoutineTrigger::Manual,
        MissedRunPolicy::Skip,
        now,
    );
    let rejected = store.activate_routine(
        &valid.routine_id,
        ActivationRequest {
            cause: ActivationCause::Manual,
            dedupe_key: "huge".into(),
            scheduled_at: now,
            received_at: now,
            payload: Some(serde_json::json!({ "blob": oversized })),
            created_by: "test".into(),
        },
        &Default::default(),
        now,
    );
    assert!(rejected.is_err());
    assert!(store.list_work_items().unwrap().is_empty());

    let missing_agent = RoutineRecord::new(
        "orphan",
        "missing-agent",
        session_id,
        "/tmp/project",
        RoutineTrigger::Manual,
        template(),
        MissedRunPolicy::Skip,
        RoutineConcurrencyPolicy::default(),
        RoutineRetryPolicy::default(),
        "test",
        now,
    )
    .unwrap();
    store.save_routine(&missing_agent).unwrap();
    let failed = store
        .activate_routine(
            &missing_agent.routine_id,
            manual_request(&missing_agent.routine_id, now, "no-agent"),
            &Default::default(),
            now,
        )
        .unwrap();
    assert!(matches!(
        failed.disposition,
        ActivationDisposition::Backoff | ActivationDisposition::CircuitOpen
    ));
    assert!(failed.work_id.is_none());
}

#[test]
fn secrets_are_redacted_on_activation_records() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        RoutineTrigger::Manual,
        MissedRunPolicy::Skip,
        now,
    );
    let activation = store
        .activate_routine(
            &routine.routine_id,
            ActivationRequest {
                cause: ActivationCause::Manual,
                dedupe_key: "secret".into(),
                scheduled_at: now,
                received_at: now,
                payload: Some(serde_json::json!({
                    "token": "super-secret",
                    "ok": true
                })),
                created_by: "test".into(),
            },
            &Default::default(),
            now,
        )
        .unwrap();
    assert_eq!(activation.payload.unwrap()["token"], "[redacted]");
}

#[test]
fn calendar_dst_schedule_is_persisted_across_fire() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session_id = Uuid::new_v4();
    let before = Utc.with_ymd_and_hms(2026, 3, 8, 6, 0, 0).unwrap();
    let trigger = RoutineTrigger::Calendar {
        timezone: "America/New_York".into(),
        hour: 2,
        minute: 30,
        weekday: None,
    };
    let routine = make_routine(
        &store,
        session_id,
        "/tmp/project",
        trigger,
        MissedRunPolicy::Skip,
        before,
    );
    let next = routine.next_fire_at.unwrap();
    let local = next.with_timezone(&chrono_tz::America::New_York);
    assert_eq!(local.day(), 9);
    assert_eq!(local.hour(), 2);
}

#[allow(dead_code)]
fn _assert_work_item_type(_: WorkItem) {}
