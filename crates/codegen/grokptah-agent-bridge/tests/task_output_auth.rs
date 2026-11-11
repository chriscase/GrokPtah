//! A caller who only knows another task id must not observe or cancel it.
//!
//! These assertions go through `dispatch_agent_tool`. They fail while
//! `task_output` / `kill_task` read or cancel by id before the session check.

use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, BackgroundTask, HostConfig, RuntimeHome, SessionKind,
    SubagentExecutionMode, SubagentInfo,
};
use uuid::Uuid;

const EMPTY_LIST: &str = "(no background tasks or subagents)";

fn denial(id: &str) -> String {
    format!("ERROR: unknown task/subagent id {id}")
}

fn same_denial(left: &str, left_id: &str, right: &str, right_id: &str) {
    assert_eq!(
        left,
        denial(left_id),
        "known id must use the unknown-id denial"
    );
    assert_eq!(right, denial(right_id));
    assert_eq!(
        left.replacen(left_id, "ID", 1),
        right.replacen(right_id, "ID", 1),
        "a present id and a missing id must be byte-identical aside from the id"
    );
}

fn task_row(
    id: &str,
    session: Option<Uuid>,
    title: &str,
    detail: &str,
    witness: u64,
) -> BackgroundTask {
    BackgroundTask {
        id: id.to_string(),
        title: title.to_string(),
        status: "running".into(),
        scheduled: true,
        kind: "scan".into(),
        session_id: session.map(|id| id.to_string()),
        detail: Some(detail.to_string()),
        witness_generation: witness,
    }
}

fn subagent_row(
    id: &str,
    session: Option<Uuid>,
    title: &str,
    cwd: &str,
    summary: &str,
    witness: u64,
) -> SubagentInfo {
    SubagentInfo {
        id: id.to_string(),
        kind: "explore".into(),
        title: title.to_string(),
        status: "running".into(),
        session_id: session.map(|id| id.to_string()),
        summary: Some(summary.to_string()),
        last_tool: None,
        cwd: Some(cwd.to_string()),
        execution_mode: SubagentExecutionMode::Worktree,
        witness_generation: witness,
    }
}

struct HomeGuard;

impl Drop for HomeGuard {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
    }
}

#[tokio::test]
async fn a_foreign_task_id_cannot_be_read_or_cancelled() {
    let tmp = tempfile::tempdir().expect("home");
    let home = tmp.path().join(".grokptah");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    let runtime = RuntimeHome::from_path(&home).expect("runtime home");
    set_grokptah_home_override(Some(runtime.path().to_path_buf()));
    let _home = HomeGuard;

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the instance lock");
    host.start().expect("start");
    let owner = host
        .session_new_kind(SessionKind::Build)
        .expect("owner session")
        .id;
    let foreign = host
        .session_new_kind(SessionKind::Build)
        .expect("foreign session")
        .id;
    let other = host
        .session_new_kind(SessionKind::Build)
        .expect("other owner")
        .id;

    host.plant_owned_work_for_test(
        Some(task_row(
            "task-a",
            Some(owner),
            "owner-only-title-481",
            "owner-only-detail-481",
            4,
        )),
        Some(subagent_row(
            "sub-a",
            Some(owner),
            "owner-only-subagent-title-481",
            "/secret/owner-only-cwd-481",
            "owner-only-summary-481",
            9,
        )),
    );
    host.plant_owned_work_for_test(
        Some(task_row(
            "unbound-task",
            None,
            "unbound-title-481",
            "unbound-detail-481",
            3,
        )),
        None,
    );
    host.plant_owned_work_for_test(
        Some(task_row(
            "shared-id",
            Some(owner),
            "collision-task-title-481",
            "collision-task-detail-481",
            2,
        )),
        Some(subagent_row(
            "shared-id",
            Some(other),
            "collision-sub-title-481",
            "/secret/collision-cwd-481",
            "collision-summary-481",
            6,
        )),
    );

    let secrets = [
        "owner-only-title-481",
        "owner-only-detail-481",
        "owner-only-subagent-title-481",
        "/secret/owner-only-cwd-481",
        "owner-only-summary-481",
        "unbound-title-481",
        "unbound-detail-481",
        "collision-task-title-481",
        "collision-task-detail-481",
        "collision-sub-title-481",
        "/secret/collision-cwd-481",
        "collision-summary-481",
    ];

    let foreign_task = host
        .dispatch_named_tool_for_test(foreign, "task_output", r#"{"id":"task-a"}"#)
        .await
        .unwrap();
    let foreign_task_alias = host
        .dispatch_named_tool_for_test(foreign, "get_task_output", r#"{"task_id":"task-a"}"#)
        .await
        .unwrap();
    let missing = host
        .dispatch_named_tool_for_test(foreign, "task_output", r#"{"id":"missing-id"}"#)
        .await
        .unwrap();
    same_denial(&foreign_task, "task-a", &missing, "missing-id");
    assert_eq!(foreign_task_alias, foreign_task);

    let foreign_sub = host
        .dispatch_named_tool_for_test(foreign, "get_task_output", r#"{"id":"sub-a"}"#)
        .await
        .unwrap();
    let foreign_unbound = host
        .dispatch_named_tool_for_test(foreign, "task_output", r#"{"id":"unbound-task"}"#)
        .await
        .unwrap();
    let foreign_shared = host
        .dispatch_named_tool_for_test(foreign, "task_output", r#"{"id":"shared-id"}"#)
        .await
        .unwrap();
    assert_eq!(foreign_sub, denial("sub-a"));
    assert_eq!(foreign_unbound, denial("unbound-task"));
    assert_eq!(foreign_shared, denial("shared-id"));

    let foreign_list = host
        .dispatch_named_tool_for_test(foreign, "task_output", "{}")
        .await
        .unwrap();
    assert_eq!(foreign_list, EMPTY_LIST);
    for secret in secrets {
        assert!(
            !foreign_task.contains(secret)
                && !foreign_sub.contains(secret)
                && !foreign_shared.contains(secret)
                && !foreign_list.contains(secret),
            "foreign output leaked {secret}"
        );
    }

    let owned_task = host
        .dispatch_named_tool_for_test(owner, "task_output", r#"{"id":"task-a"}"#)
        .await
        .unwrap();
    assert!(owned_task.contains("owner-only-detail-481"));
    let owned_sub = host
        .dispatch_named_tool_for_test(owner, "get_task_output", r#"{"task_id":"sub-a"}"#)
        .await
        .unwrap();
    assert!(owned_sub.contains("/secret/owner-only-cwd-481"));
    assert!(owned_sub.contains("owner-only-summary-481"));
    let owned_list = host
        .dispatch_named_tool_for_test(owner, "task_output", "{}")
        .await
        .unwrap();
    assert!(owned_list.contains("owner-only-title-481"));
    assert!(owned_list.contains("/secret/owner-only-cwd-481"));
    assert!(!owned_list.contains("unbound-title-481"));
    assert!(!owned_list.contains("collision-sub-title-481"));
    assert!(!owned_list.contains("/secret/collision-cwd-481"));

    let other_shared = host
        .dispatch_named_tool_for_test(other, "task_output", r#"{"id":"shared-id"}"#)
        .await
        .unwrap();
    assert!(other_shared.contains("/secret/collision-cwd-481"));
    assert!(other_shared.contains("collision-summary-481"));
    assert!(!other_shared.contains("collision-task-detail-481"));
    let owner_shared = host
        .dispatch_named_tool_for_test(owner, "get_task_output", r#"{"id":"shared-id"}"#)
        .await
        .unwrap();
    assert!(owner_shared.contains("collision-task-detail-481"));
    assert!(!owner_shared.contains("/secret/collision-cwd-481"));

    let owned_witness = host.witnessed_tool_output_for_test(
        owner,
        "task_output",
        r#"{"id":"task-a"}"#,
        &owned_task,
    );
    assert!(
        owned_witness.starts_with("host-witness:task-a:"),
        "an owned wait must stay a stationarity witness, got {owned_witness}"
    );
    assert!(!owned_witness.contains("owner-only-detail-481"));
    let foreign_witness = host.witnessed_tool_output_for_test(
        foreign,
        "get_task_output",
        r#"{"task_id":"task-a"}"#,
        &foreign_task,
    );
    assert_eq!(foreign_witness, foreign_task);
    assert!(!foreign_witness.contains("host-witness"));

    for (tool, args) in [
        ("kill_task", r#"{"id":"task-a"}"#),
        ("cancel_task", r#"{"task_id":"sub-a"}"#),
        ("kill_task", r#"{"id":"shared-id"}"#),
        ("cancel_task", r#"{"id":"unbound-task"}"#),
        ("kill_task", r#"{"id":"missing-id"}"#),
    ] {
        let denied = host
            .dispatch_named_tool_for_test(foreign, tool, args)
            .await
            .unwrap();
        let id = if args.contains("sub-a") {
            "sub-a"
        } else if args.contains("shared-id") {
            "shared-id"
        } else if args.contains("unbound-task") {
            "unbound-task"
        } else if args.contains("missing-id") {
            "missing-id"
        } else {
            "task-a"
        };
        assert_eq!(denied, denial(id));
        for secret in secrets {
            assert!(!denied.contains(secret), "foreign cancel leaked {secret}");
        }
    }
    assert_task(&host, "task-a", "running", "owner-only-detail-481", 4);
    assert_subagent(
        &host,
        "sub-a",
        "running",
        "owner-only-summary-481",
        "/secret/owner-only-cwd-481",
        9,
    );
    assert_task(
        &host,
        "shared-id",
        "running",
        "collision-task-detail-481",
        2,
    );
    assert_subagent(
        &host,
        "shared-id",
        "running",
        "collision-summary-481",
        "/secret/collision-cwd-481",
        6,
    );
    assert_task(&host, "unbound-task", "running", "unbound-detail-481", 3);

    let owner_cancels_shared = host
        .dispatch_named_tool_for_test(owner, "kill_task", r#"{"id":"shared-id"}"#)
        .await
        .unwrap();
    assert_eq!(owner_cancels_shared, "killed background task shared-id");
    assert_task(&host, "shared-id", "cancelled", "cancelled by user", 3);
    assert_subagent(
        &host,
        "shared-id",
        "running",
        "collision-summary-481",
        "/secret/collision-cwd-481",
        6,
    );

    let other_cancels_shared = host
        .dispatch_named_tool_for_test(other, "cancel_task", r#"{"id":"shared-id"}"#)
        .await
        .unwrap();
    assert_eq!(other_cancels_shared, "cancelled subagent shared-id");
    assert_task(&host, "shared-id", "cancelled", "cancelled by user", 3);
    assert_subagent(
        &host,
        "shared-id",
        "cancelled",
        "cancelled by user",
        "/secret/collision-cwd-481",
        7,
    );

    let owner_cancels_sub = host
        .dispatch_named_tool_for_test(owner, "cancel_task", r#"{"task_id":"sub-a"}"#)
        .await
        .unwrap();
    assert_eq!(owner_cancels_sub, "cancelled subagent sub-a");
    assert_subagent(
        &host,
        "sub-a",
        "cancelled",
        "cancelled by user",
        "/secret/owner-only-cwd-481",
        10,
    );
    let owner_cancels_task = host
        .dispatch_named_tool_for_test(owner, "kill_task", r#"{"id":"task-a"}"#)
        .await
        .unwrap();
    assert_eq!(owner_cancels_task, "killed background task task-a");
    assert_task(&host, "task-a", "cancelled", "cancelled by user", 5);

    let empty = host
        .dispatch_named_tool_for_test(owner, "kill_task", r#"{"id":""}"#)
        .await
        .unwrap();
    assert_eq!(empty, "ERROR: kill_task requires id");
    assert_task(&host, "unbound-task", "running", "unbound-detail-481", 3);

    let report = host.shutdown().await;
    assert!(report.is_clean(), "{}", report.operator_summary());
}

fn assert_task(
    host: &grokptah_agent_bridge::HostRuntime,
    id: &str,
    status: &str,
    detail: &str,
    witness: u64,
) {
    let task = host
        .background_tasks()
        .into_iter()
        .find(|task| task.id == id)
        .unwrap_or_else(|| panic!("missing task {id}"));
    assert_eq!(task.status, status);
    assert_eq!(task.detail.as_deref(), Some(detail));
    assert_eq!(task.witness_generation, witness);
}

fn assert_subagent(
    host: &grokptah_agent_bridge::HostRuntime,
    id: &str,
    status: &str,
    summary: &str,
    cwd: &str,
    witness: u64,
) {
    let subagent = host
        .subagents()
        .into_iter()
        .find(|subagent| subagent.id == id)
        .unwrap_or_else(|| panic!("missing subagent {id}"));
    assert_eq!(subagent.status, status);
    assert_eq!(subagent.summary.as_deref(), Some(summary));
    assert_eq!(subagent.cwd.as_deref(), Some(cwd));
    assert_eq!(subagent.witness_generation, witness);
}
