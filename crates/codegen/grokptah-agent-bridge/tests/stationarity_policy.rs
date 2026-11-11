//! Production policy for #465 that does not rebuild the identical-tool detector.
//!
//! The restart test uses a local host and a durable ledger only. It does not
//! call a provider.

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, RunBounds, RunPurpose, RunRecord, RunState, RunStopCause,
};
use grokptah_agent_bridge::{
    apply_durable_stationarity_stop, dispatch_may_retry_automatically, note_wait_witness,
    set_grokptah_home_override, AgentHost, HostConfig, RuntimeHome, SessionKind,
    HOST_WAIT_DEADLINE_MS,
};
use uuid::Uuid;

fn running_run() -> RunRecord {
    let now = Utc::now();
    RunRecord {
        run_id: "run-1".into(),
        session_id: Uuid::nil(),
        workspace: "/tmp/project".into(),
        request_id: "request-1".into(),
        client_id: None,
        state: RunState::Running,
        purpose: RunPurpose::Execution,
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: None,
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: String::new(),
        start_seq: None,
        end_seq: None,
        created_at: now,
        updated_at: now,
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
fn a_stationary_stop_closes_a_live_run_without_retrying_it() {
    let mut live = running_run();
    assert!(apply_durable_stationarity_stop(&mut live));
    assert_eq!(live.state, RunState::LimitReached);
    assert_eq!(live.stop_cause, Some(RunStopCause::Stationarity));
    assert!(live.retry_of.is_none());
    assert_eq!(live.state, RunState::LimitReached);
    assert!(!apply_durable_stationarity_stop(&mut live));
}

#[test]
fn a_wait_is_productive_only_while_the_host_witness_advances() {
    let (admitted, productive) =
        note_wait_witness(None, "task-9", 2, 5_000, "waiting on the build");
    assert!(!productive, "the admitting poll is not progress");
    assert_eq!(admitted.deadline_unix_ms, 5_000 + HOST_WAIT_DEADLINE_MS);

    let (same, productive) = note_wait_witness(
        Some(admitted.clone()),
        "task-9",
        2,
        6_000,
        "the model says the log grew",
    );
    assert!(!productive);
    assert_eq!(same.deadline_unix_ms, admitted.deadline_unix_ms);

    let (moved, productive) = note_wait_witness(Some(same), "task-9", 3, 7_000, "still just prose");
    assert!(productive);
    assert_eq!(moved.deadline_unix_ms, admitted.deadline_unix_ms);
    assert_eq!(moved.admitted_generation, 3);

    let (_late, productive) = note_wait_witness(
        Some(moved),
        "task-9",
        9,
        admitted.deadline_unix_ms,
        "generation moved after the deadline",
    );
    assert!(!productive);
}

#[test]
fn an_uncertain_dispatch_cannot_retry_even_if_marked_safe_to_resend() {
    assert!(!dispatch_may_retry_automatically(true, true));
    assert!(dispatch_may_retry_automatically(false, true));
}

#[tokio::test]
async fn a_restart_applies_the_durable_stop_and_does_not_dispatch() {
    let tmp = tempfile::tempdir().expect("home");
    let home = tmp.path().join(".grokptah");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    let runtime = RuntimeHome::from_path(&home).expect("runtime home");
    set_grokptah_home_override(Some(runtime.path().to_path_buf()));

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the instance lock");
    host.start().expect("start");
    let session = host.session_new_kind(SessionKind::Build).expect("session");
    let session_id = session.id;
    let report = host.shutdown().await;
    assert!(report.is_clean(), "{}", report.operator_summary());
    drop(host);

    let meta_path = runtime
        .path()
        .join("sessions")
        .join(session_id.to_string())
        .join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta["stationary_stop"] = serde_json::json!({
        "revision": 1,
        "code": "stationarity",
        "toolName": "read_file",
        "runLen": 16,
        "trueNoop": false
    });
    meta["wait_witnesses"] = serde_json::json!([{
        "taskId": "task-9",
        "admittedGeneration": 2,
        "deadlineUnixMs": 5_000 + HOST_WAIT_DEADLINE_MS
    }]);
    std::fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();

    {
        let store = OrchStore::open(runtime.orchestration_root()).unwrap();
        let mut open = running_run();
        open.run_id = "open-stationary".into();
        open.session_id = session_id;
        store.save_run(&open).unwrap();
        let mut done = running_run();
        done.run_id = "already-finished".into();
        done.session_id = session_id;
        done.state = RunState::Completed;
        done.terminal_result = Some("completed".into());
        store.save_run(&done).unwrap();
    }

    let restarted = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("replacement instance lock");
    restarted.start().expect("restart");
    let store = restarted.ensure_orchestration_store().unwrap();
    let open = store.load_run("open-stationary").unwrap().unwrap();
    assert_eq!(open.state, RunState::LimitReached);
    assert_eq!(open.stop_cause, Some(RunStopCause::Stationarity));
    assert_eq!(open.error_code.as_deref(), Some("stationarity"));
    assert!(open.retry_of.is_none());
    let done = store.load_run("already-finished").unwrap().unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert!(done.retry_of.is_none());

    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert!(meta.get("stationary_stop").is_none());
    assert_eq!(
        meta["wait_witnesses"][0]["admittedGeneration"].as_u64(),
        Some(2)
    );
    let restart_report = restarted.shutdown().await;
    assert!(
        restart_report.is_clean(),
        "{}",
        restart_report.operator_summary()
    );
    set_grokptah_home_override(None);
}
