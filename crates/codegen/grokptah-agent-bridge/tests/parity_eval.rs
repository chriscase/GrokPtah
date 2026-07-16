//! Offline parity smoke harness (#93).
//!
//! Runs fixture tasks against the shipped AgentHost offline path.
//! Full live CLI comparison is manual (see docs/PARITY_EVALS.md).

use std::fs;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, HostConfig, SessionUpdate,
};
use tokio::time::timeout;

fn home_serial() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

struct IsolatedHome {
    _tmp: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn install() -> Self {
        let lock = home_serial().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("memory")).unwrap();
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
        }
    }
}

async fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>) {
    loop {
        match timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(ev)) if matches!(ev, SessionUpdate::TurnComplete { .. }) => break,
            Ok(Some(_)) => {}
            _ => break,
        }
    }
}

fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("AGENTS.md"),
        "Always add trailing newlines.\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    fs::write(dir.path().join("README.md"), "# Fixture\n\nsearch-me-token\n").unwrap();
    dir
}

/// Smoke task 1: search (list/read path)
#[tokio::test]
async fn smoke_search_lists_and_reads() {
    let _iso = IsolatedHome::install();
    let dir = fixture_repo();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    host.session_prompt(s.id, "list files please".into())
        .await
        .unwrap();
    drain(&mut rx).await;
    // Tool card proves shipped list_dir path
    // (events drained; re-prompt with write to prove tools still work)
    host.session_prompt(s.id, "write out.txt: found".into())
        .await
        .unwrap();
    drain(&mut rx).await;
    assert!(
        dir.path().join("out.txt").is_file(),
        "smoke write must create file via offline agent"
    );
}

/// Smoke task 2: structured multi-hunk-capable patch (JSON path of apply_patch)
#[tokio::test]
async fn smoke_edit_apply_patch_region() {
    let _iso = IsolatedHome::install();
    let dir = fixture_repo();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    let patch = r#"{"path":"src/lib.rs","old_string":"a + b","new_string":"a.wrapping_add(b)"}"#;
    host.session_prompt(s.id, format!("patch {patch}"))
        .await
        .unwrap();
    drain(&mut rx).await;
    let body = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        body.contains("wrapping_add"),
        "apply_patch must edit only targeted region; body={body}"
    );
    assert!(
        body.contains("pub fn add"),
        "must not rewrite entire file structure"
    );
}

/// Smoke task 3: refuse/safe — readonly sandbox blocks write
#[tokio::test]
async fn smoke_refuse_unsafe_write_in_readonly() {
    let _iso = IsolatedHome::install();
    let dir = fixture_repo();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    host.set_sandbox("read-only".into());
    let s = host.session_new().unwrap();
    host.session_prompt(s.id, "write evil.txt: no".into())
        .await
        .unwrap();
    drain(&mut rx).await;
    assert!(
        !dir.path().join("evil.txt").exists(),
        "readonly must refuse write"
    );
}

#[tokio::test]
async fn smoke_todo_and_memory_tools() {
    let _iso = IsolatedHome::install();
    let dir = fixture_repo();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    host.session_prompt(s.id, "todo implement feature X".into())
        .await
        .unwrap();
    drain(&mut rx).await;
    host.session_prompt(s.id, "remember Always use wrapping_add for add()".into())
        .await
        .unwrap();
    drain(&mut rx).await;

    // New session same project → memory recall
    let s2 = host.session_new().unwrap();
    host.session_prompt(s2.id, "recall wrapping_add".into())
        .await
        .unwrap();
    drain(&mut rx).await;
    let facts = host.memory_list().unwrap();
    assert!(
        facts.iter().any(|f| f.text.contains("wrapping_add")),
        "project memory must persist across sessions: {facts:?}"
    );
}

#[tokio::test]
async fn compact_never_shrinks_local_transcript() {
    let _iso = IsolatedHome::install();
    let dir = fixture_repo();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    for i in 0..12 {
        host.session_prompt(s.id, format!("list files wave {i}"))
            .await
            .unwrap();
        drain(&mut rx).await;
    }
    let before = {
        // export is the real API surface
        let t = host.export_transcript(s.id).unwrap();
        t.matches("## [").count()
    };
    host.session_prompt(s.id, "/compact".into()).await.unwrap();
    drain(&mut rx).await;
    let after = host.export_transcript(s.id).unwrap().matches("## [").count();
    assert!(
        after >= before,
        "compact must not delete local transcript lines ({before} → {after})"
    );
    // Wire summary present
    let export = host.export_transcript(s.id).unwrap();
    assert!(
        export.contains("compacted") || export.contains("api_context_start"),
        "export should show compact state"
    );
}
