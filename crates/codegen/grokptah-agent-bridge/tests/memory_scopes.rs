//! Focused acceptance coverage for durable source-workspace memory (#298).

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::{
    AuthContext, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunExecutionMode,
    RunState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig,
    MemoryScope,
};

struct IsolatedProcess {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: tempfile::TempDir,
}

impl IsolatedProcess {
    fn install() -> Self {
        let serial = home_override_serial();
        let root = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(root.path().join(".grokptah")));
        unsafe {
            std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
            std::env::set_var("GROKPTAH_MCP_SKIP", "1");
        }
        Self {
            _serial: serial,
            root,
        }
    }

    fn home(&self) -> std::path::PathBuf {
        self.root.path().join(".grokptah")
    }
}

impl Drop for IsolatedProcess {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        unsafe {
            std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
            std::env::remove_var("GROKPTAH_MCP_SKIP");
        }
    }
}

fn started_host(workspace: &Path) -> AgentHostHandle {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace).unwrap();
    host
}

fn git(workspace: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_repo() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "-b", "main"]);
    git(
        workspace.path(),
        &["config", "user.email", "memory@test.invalid"],
    );
    git(workspace.path(), &["config", "user.name", "Memory Test"]);
    fs::write(workspace.path().join("README.md"), "baseline\n").unwrap();
    git(workspace.path(), &["add", "README.md"]);
    git(workspace.path(), &["commit", "-m", "baseline"]);
    workspace
}

async fn wait_terminal(
    service: &OrchestrationService,
    auth: &AuthContext,
    run_id: &str,
) -> RunState {
    let started = Instant::now();
    loop {
        let run = service.get_run(auth, run_id).unwrap();
        let state: RunState = serde_json::from_value(run["state"].clone()).unwrap();
        if state.is_terminal() {
            return state;
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn project_scope_matches_desktop_service_and_isolated_model_tools_across_restart() {
    let process = IsolatedProcess::install();
    let workspace = fixture_repo();
    let host = started_host(workspace.path());
    let session = host.session_new().unwrap();
    let agent = host.ensure_session_agent(session.id).unwrap();

    // Desktop caller: explicit session plus explicit project scope.
    host.memory_remember(session.id, MemoryScope::Project, "desktop project fact")
        .unwrap();

    // Standalone-service caller: the service dispatches the model tool through
    // the same host session. The isolated execution cwd must not become the
    // memory address.
    let service = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(process.home().join("service-orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "memory-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    let auth = service.auth_header(Some("Bearer memory-token")).unwrap();
    let submitted = service
        .submit_task_with_execution_mode(
            &auth,
            "memory-isolated-service",
            session.id,
            workspace.path(),
            "remember isolated service fact".into(),
            None,
            RunExecutionMode::IsolatedWorktree,
        )
        .await
        .unwrap();
    let run_id = submitted["runId"].as_str().unwrap();
    assert_eq!(
        wait_terminal(&service, &auth, run_id).await,
        RunState::Completed
    );

    let facts = host.memory_list(session.id, MemoryScope::Project).unwrap();
    assert_eq!(
        facts
            .iter()
            .map(|fact| fact.text.as_str())
            .collect::<Vec<_>>(),
        vec!["desktop project fact", "isolated service fact"]
    );

    // Discard removes only the execution worktree; the completed memory tool
    // write remains durable at the source-workspace address.
    host.discard_run(session.id, run_id).unwrap();
    assert_eq!(
        host.memory_list(session.id, MemoryScope::Project)
            .unwrap()
            .len(),
        2
    );

    // A promoted isolated code run likewise does not own memory lifetime.
    host.session_set_execution_mode(session.id, RunExecutionMode::IsolatedWorktree)
        .unwrap();
    host.session_prompt(session.id, "write promoted.txt: promoted content".into())
        .await
        .unwrap();
    let promoted_run = host
        .list_session_runs(session.id)
        .unwrap()
        .into_iter()
        .filter(|run| run.run_id != run_id)
        .max_by_key(|run| run.updated_at)
        .unwrap();
    host.memory_remember(
        session.id,
        MemoryScope::Project,
        "fact retained through promotion",
    )
    .unwrap();
    host.review_run(session.id, &promoted_run.run_id).unwrap();
    host.promote_run(session.id, &promoted_run.run_id).unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path().join("promoted.txt")).unwrap(),
        "promoted content"
    );

    // Agent-private resolution checks the stable binding, and team scope is
    // schema-valid but policy-denied until an approved sharing policy exists.
    host.memory_remember(
        session.id,
        MemoryScope::AgentPrivate {
            agent_id: agent.agent_id.clone(),
        },
        "first agent private fact",
    )
    .unwrap();
    let second = host.session_new().unwrap();
    let second_agent = host.ensure_session_agent(second.id).unwrap();
    assert!(host
        .memory_list(
            second.id,
            MemoryScope::AgentPrivate {
                agent_id: agent.agent_id.clone(),
            },
        )
        .is_err());
    assert!(host
        .memory_list(
            second.id,
            MemoryScope::AgentPrivate {
                agent_id: second_agent.agent_id,
            },
        )
        .unwrap()
        .is_empty());
    assert!(host
        .memory_list(
            session.id,
            MemoryScope::Team {
                team_id: "unapproved-team".into(),
            },
        )
        .is_err());

    drop(service);
    drop(host);

    // Reopening the same GrokPtah home and durable session proves the source
    // address and all completed writes survive a process boundary.
    let restarted = started_host(workspace.path());
    let restarted_facts = restarted
        .memory_list(session.id, MemoryScope::Project)
        .unwrap();
    let texts: Vec<_> = restarted_facts.into_iter().map(|fact| fact.text).collect();
    assert_eq!(
        texts,
        vec![
            "desktop project fact",
            "isolated service fact",
            "fact retained through promotion",
        ]
    );
}
