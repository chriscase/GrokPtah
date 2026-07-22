//! #162 / #172 multi-agent isolation + capability modes (shipped paths).
//!
//! Tests drive AgentHost + offline GP body (the real shipped path under
//! GROKPTAH_AGENT_OFFLINE), not reimplemented isolation/write logic.

#![allow(clippy::while_let_loop)]

use std::fs;
use std::time::Duration;

use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHost, HostConfig, SessionUpdate,
};
use tokio::time::timeout;

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

async fn wait_subagents_done(rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>, n: u32) {
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

/// #162: two isolated GP children write different paths without colliding.
///
/// Asserts (not theater):
/// - each write lands under a *distinct* `.grokptah/worktrees/sub-*` tree
/// - parent project root does **not** receive either write
#[tokio::test]
async fn isolation_two_children_writes_do_not_collide() {
    let _iso = IsolatedHome::install();
    unsafe {
        std::env::set_var("GROKPTAH_SUBAGENT_ISOLATION", "worktree");
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "root\n").unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let a = host
        .spawn_subagent_public(session.id, "general-purpose", "write a.txt: child-a")
        .await
        .unwrap();
    let b = host
        .spawn_subagent_public(session.id, "general-purpose", "write b.txt: child-b")
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
        !dir.path().join("a.txt").exists(),
        "parent must not contain a.txt (isolation leak)"
    );
    assert!(
        !dir.path().join("b.txt").exists(),
        "parent must not contain b.txt (isolation leak)"
    );

    let wt = dir.path().join(".grokptah").join("worktrees");
    assert!(wt.is_dir(), "expected .grokptah/worktrees");

    let mut wt_with_a: Option<std::path::PathBuf> = None;
    let mut wt_with_b: Option<std::path::PathBuf> = None;
    for e in fs::read_dir(&wt).unwrap().flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy();
        if !name.starts_with("sub-") {
            continue;
        }
        // Isolation tree must still have project files
        assert!(
            p.join("README.md").is_file(),
            "isolation cwd empty or incomplete: {}",
            p.display()
        );
        if p.join("a.txt").is_file() {
            let body = fs::read_to_string(p.join("a.txt")).unwrap();
            assert!(
                body.contains("child-a"),
                "a.txt body in {}: {body}",
                p.display()
            );
            wt_with_a = Some(p.clone());
        }
        if p.join("b.txt").is_file() {
            let body = fs::read_to_string(p.join("b.txt")).unwrap();
            assert!(
                body.contains("child-b"),
                "b.txt body in {}: {body}",
                p.display()
            );
            wt_with_b = Some(p.clone());
        }
    }

    let wa = wt_with_a.expect("child-a write must land under a sub-* worktree");
    let wb = wt_with_b.expect("child-b write must land under a sub-* worktree");
    assert_ne!(
        wa,
        wb,
        "a.txt and b.txt must be in distinct isolation worktrees (got both in {})",
        wa.display()
    );
    // Cross-check: neither worktree should hold the other's file
    assert!(
        !wa.join("b.txt").exists(),
        "worktree A must not contain b.txt"
    );
    assert!(
        !wb.join("a.txt").exists(),
        "worktree B must not contain a.txt"
    );
}

/// #161: plan offline path refuses write_ even when prompt requests it.
#[tokio::test]
async fn plan_kind_denies_write_offline() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("keep.txt"), "safe").unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

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

    // GP control on same host (one instance lock): general-purpose *does* write.
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
async fn prepare_isolation_never_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("marker.txt"), "present").unwrap();
    let wt = grokptah_agent_bridge::prepare_isolation_cwd(dir.path(), "unit-1").unwrap();
    assert!(wt.join("marker.txt").is_file());
    let body = fs::read_to_string(wt.join("marker.txt")).unwrap();
    assert_eq!(body, "present");
}
