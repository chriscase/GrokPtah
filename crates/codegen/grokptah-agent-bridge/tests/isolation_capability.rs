//! #162 / #172 multi-agent isolation + capability modes (shipped paths).
//!
//! Tests drive AgentHost + offline GP body (the real shipped path under
//! GROKPTAH_AGENT_OFFLINE), not reimplemented isolation/write logic.

#![allow(clippy::while_let_loop)]

use std::fs;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHost, HostConfig, SessionUpdate,
    SubagentExecutionMode,
};
use tokio::time::timeout;

fn provisioned_host(config: HostConfig) -> grokptah_agent_bridge::AgentHostHandle {
    let host = AgentHost::create(config);
    let store = host
        .ensure_orchestration_store()
        .expect("open canonical orchestration store");
    let _authority_service = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "isolation-authority".into(),
            allowlist: WorkspaceAllowlist::default(),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    host
}

struct IsolatedHome {
    _tmp: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn install() -> Self {
        let lock = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        fs::create_dir_all(home.join("sessions")).unwrap();
        set_grokptah_home_override(Some(home));
        unsafe {
            std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
        }
        Self {
            _tmp: tmp,
            _lock: lock,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        unsafe {
            std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
            std::env::remove_var("GROKPTAH_SUBAGENT_ISOLATION");
        }
    }
}

async fn wait_subagents_done(rx: &mut grokptah_agent_bridge::EventReceiver, n: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut done = 0u32;
    while done < n && tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(400), rx.recv()).await {
            Ok(Some(SessionUpdate::SubagentUpdate { status, .. }))
                if matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
            {
                done += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(done >= n, "expected {n} finished subagents, got {done}");
}

/// #162 / #195: two default GP children can write the same path without colliding.
///
/// Asserts (not theater):
/// - each write lands under a *distinct* `.grokptah/worktrees/sub-*` tree
/// - parent project root does **not** receive the write
#[tokio::test]
async fn default_isolation_keeps_two_mutating_children_from_colliding() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "root\n").unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();

    let host = provisioned_host(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    host.ensure_session_agent(session.id)
        .expect("bind canonical Agent authority");

    let a = host
        .spawn_subagent_public(
            session.id,
            "general-purpose",
            "write collision.txt: child-a",
        )
        .await
        .unwrap();
    let b = host
        .spawn_subagent_public(
            session.id,
            "general-purpose",
            "write collision.txt: child-b",
        )
        .await
        .unwrap();
    assert!(
        !a.contains("isolation failed") && a.contains("Spawned"),
        "child A: {a}"
    );
    assert!(
        !b.contains("isolation failed") && b.contains("Spawned"),
        "child B: {b}"
    );

    wait_subagents_done(&mut rx, 2).await;

    // Parent must NOT have the writes
    assert!(
        !dir.path().join("collision.txt").exists(),
        "parent must not contain collision.txt (isolation leak)"
    );

    let children: Vec<_> = host
        .subagents()
        .into_iter()
        .filter(|child| child.session_id.as_deref() == Some(&session.id.to_string()))
        .collect();
    assert_eq!(children.len(), 2);
    let mut worktree_a = None;
    let mut worktree_b = None;
    for child in children {
        assert_eq!(child.execution_mode, SubagentExecutionMode::ProjectCopy);
        let cwd = child.cwd.map(std::path::PathBuf::from).expect("child cwd");
        assert!(
            cwd.join("README.md").is_file(),
            "isolation cwd empty or incomplete: {}",
            cwd.display()
        );
        let body = fs::read_to_string(cwd.join("collision.txt")).unwrap();
        if body.contains("child-a") {
            worktree_a = Some(cwd);
        } else if body.contains("child-b") {
            worktree_b = Some(cwd);
        } else {
            panic!("unexpected collision.txt body: {body}");
        }
    }

    let wa = worktree_a.expect("child-a write must land under a sub-* worktree");
    let wb = worktree_b.expect("child-b write must land under a sub-* worktree");
    assert_ne!(
        wa,
        wb,
        "same-path writes must land in distinct isolation worktrees (got {})",
        wa.display()
    );
}

/// #161: plan offline path refuses write_ even when prompt requests it.
#[tokio::test]
async fn plan_kind_denies_write_offline() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("keep.txt"), "safe").unwrap();

    let host = provisioned_host(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    host.ensure_session_agent(session.id)
        .expect("bind canonical Agent authority");

    // Real mutator request — must be denied for plan kind (same gate as online).
    let msg = host
        .spawn_subagent_public(session.id, "plan", "write plan-leak.txt: should-not-exist")
        .await
        .unwrap();
    assert!(msg.contains("Spawned") || msg.contains("parallel"), "{msg}");

    wait_subagents_done(&mut rx, 1).await;

    assert!(
        !dir.path().join("plan-leak.txt").exists(),
        "plan must not create plan-leak.txt in project root"
    );
    // Also must not appear under any isolation tree if isolation were on
    let wt = dir.path().join(".grokptah").join("worktrees");
    if wt.is_dir() {
        for e in fs::read_dir(&wt).unwrap().flatten() {
            assert!(
                !e.path().join("plan-leak.txt").exists(),
                "plan must not write under {}",
                e.path().display()
            );
        }
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "safe"
    );
    let plan = host
        .subagents()
        .into_iter()
        .find(|child| child.kind == "plan")
        .expect("plan child");
    assert_eq!(plan.execution_mode, SubagentExecutionMode::SharedReadOnly);
    assert_eq!(
        plan.cwd.as_deref(),
        Some(dir.path().to_string_lossy().as_ref())
    );

    // GP control on same host (one instance lock): general-purpose *does* write.
    host.set_subagent_isolation("shared".into()).unwrap();
    host.spawn_subagent_public(session.id, "general-purpose", "write gp-ok.txt: from-gp")
        .await
        .unwrap();
    wait_subagents_done(&mut rx, 1).await;
    assert!(
        dir.path().join("gp-ok.txt").is_file(),
        "general-purpose control write must succeed so plan deny is meaningful"
    );
    assert!(fs::read_to_string(dir.path().join("gp-ok.txt"))
        .unwrap()
        .contains("from-gp"));
}

#[tokio::test]
async fn failed_isolation_requires_explicit_shared_cwd_opt_in() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "root\n").unwrap();
    fs::write(dir.path().join(".grokptah"), "blocks worktree directory").unwrap();

    let host = provisioned_host(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    host.ensure_session_agent(session.id)
        .expect("bind canonical Agent authority");

    let denied = host
        .spawn_subagent_public(
            session.id,
            "general-purpose",
            "write denied.txt: must-not-run",
        )
        .await
        .unwrap();
    assert!(denied.contains("isolation failed"), "{denied}");
    wait_subagents_done(&mut rx, 1).await;
    assert!(!dir.path().join("denied.txt").exists());
    let failed = host
        .subagents()
        .into_iter()
        .find(|child| child.status == "failed")
        .expect("failed child row");
    assert_eq!(
        failed.execution_mode,
        SubagentExecutionMode::IsolationFailed
    );
    assert!(
        failed.cwd.is_none(),
        "failed child must not claim parent cwd"
    );

    host.set_subagent_isolation("shared".into()).unwrap();
    let shared = host
        .spawn_subagent_public(
            session.id,
            "general-purpose",
            "write shared.txt: explicit-opt-in",
        )
        .await
        .unwrap();
    assert!(shared.contains("explicit user opt-in"), "{shared}");
    wait_subagents_done(&mut rx, 1).await;
    assert!(fs::read_to_string(dir.path().join("shared.txt"))
        .unwrap()
        .contains("explicit-opt-in"));
    let shared_child = host
        .subagents()
        .into_iter()
        .find(|child| child.execution_mode == SubagentExecutionMode::SharedMutating)
        .expect("shared mutating child");
    assert_eq!(
        shared_child.cwd.as_deref(),
        Some(dir.path().to_string_lossy().as_ref())
    );
    assert_eq!(
        host.settings_snapshot()["subagentIsolation"],
        serde_json::json!("shared")
    );
}

#[test]
fn shared_cwd_preference_is_explicit_and_persistent() {
    let _iso = IsolatedHome::install();
    let host = AgentHost::create(HostConfig::default());
    assert!(host.set_subagent_isolation("off".into()).is_err());
    host.set_subagent_isolation("shared".into()).unwrap();
    drop(host);

    let restored = AgentHost::create(HostConfig::default());
    assert_eq!(
        restored.settings_snapshot()["subagentIsolation"],
        serde_json::json!("shared")
    );
}

#[tokio::test]
async fn prepare_isolation_never_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("marker.txt"), "present").unwrap();
    let wt = grokptah_agent_bridge::prepare_isolation_cwd(dir.path(), "unit-1").unwrap();
    assert!(wt.join("marker.txt").is_file());
    let body = fs::read_to_string(wt.join("marker.txt")).unwrap();
    assert_eq!(body, "present");
}
