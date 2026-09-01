//! End-to-end service coverage for the managed Grok Build executor.
//!
//! The child is a deterministic fake CLI. Live provider qualification is a
//! separate, explicit gate, but this test exercises the real durable Work,
//! authorization, dispatch, supervision, adapter, mutation-evidence, and
//! finalization path.

mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    AssignmentStatus, AuthContext, ManagedExecutionBudgetProfile, ManagedExecutionPolicy,
    ManagedExecutorKind, ManagedGrokExecutorConfig, ManagedIntentState, ManagerStepSpec, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, WorkItem, WorkPolicy, WorkRetryPolicy,
    WorkState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, CredentialLeaseHandle, CredentialLeaseResolver,
    GrokBuildAdapterError, HostConfig, SessionKind,
};
use grokptah_agent_sdk::GrokBuildGitIdentity;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

#[derive(Clone)]
struct FileLeaseResolver {
    path: PathBuf,
}

impl CredentialLeaseResolver for FileLeaseResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        if lease_id != "dogfood-credential-lease" {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        Ok(CredentialLeaseHandle::from_host_path(self.path.clone()))
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        if lease_id != "dogfood-credential-lease" {
            return Err(GrokBuildAdapterError::CredentialRevocation);
        }
        if self.path.exists() {
            fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&self.path)
                .and_then(|file| file.sync_all())
                .map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
            fs::remove_file(&self.path).map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
        }
        Ok(())
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn initialize_repo(repo: &Path) -> GrokBuildGitIdentity {
    git(repo, &["init", "-b", "topic"]);
    git(repo, &["config", "user.name", "GrokPtah test"]);
    git(repo, &["config", "user.email", "test@grokptah.invalid"]);
    fs::write(repo.join("DOGFOOD.txt"), "before\n").unwrap();
    git(repo, &["add", "DOGFOOD.txt"]);
    git(repo, &["commit", "-m", "dogfood base"]);
    let head = git(repo, &["rev-parse", "HEAD"]);
    GrokBuildGitIdentity {
        repository_id: "repo-grokptah-dogfood".into(),
        git_ref: "refs/heads/topic".into(),
        base_sha: head.clone(),
        head_sha: head,
    }
}

#[cfg(unix)]
fn install_fake_grok(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, FAKE_GROK).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn grok_policy(profile: ManagedExecutionBudgetProfile) -> ManagedExecutionPolicy {
    ManagedExecutionPolicy {
        enabled: true,
        allowed_work_kinds: vec!["isolated-review".into()],
        max_concurrent_runs: 1,
        bounds: RunBounds {
            max_prompt_bytes: profile.limits().max_prompt_bytes,
            max_rounds: profile.limits().max_turns,
            max_duration_ms: profile.limits().max_duration_ms,
            max_total_tokens: Some(16_000),
        },
        retry_eligible: false,
        requires_approval_before_execution: true,
        executor: ManagedExecutorKind::GrokBuildIsolatedReview,
        budget_profile: Some(profile),
        ..ManagedExecutionPolicy::default()
    }
}

fn auth() -> AuthContext {
    AuthContext {
        token_id: "operator".into(),
        owner_id: "primary".into(),
    }
}

fn isolated_review_step(agent_id: &str, step_id: &str, objective: &str) -> ManagerStepSpec {
    ManagerStepSpec {
        step_id: step_id.into(),
        kind: "isolated-review".into(),
        objective: objective.into(),
        priority: 0,
        dependencies: Vec::new(),
        assigned_agent_id: Some(agent_id.into()),
        policy: WorkPolicy {
            retry: WorkRetryPolicy {
                max_attempts: 1,
                retry_failed: false,
                retry_expired: false,
                backoff_ms: 0,
            },
            allowed_files: vec!["DOGFOOD.txt".into()],
            ..WorkPolicy::default()
        },
    }
}

fn set_fake_behavior(fake_dir: &Path, behavior: &str) {
    fs::write(fake_dir.join("behavior"), behavior).unwrap();
}

fn assert_redacted_operator_result(work: &WorkItem, workspace: &Path) {
    let result = work.result.as_ref().expect("operator result");
    assert!(result.verification.is_none());
    let rendered = format!("{result:?}");
    assert!(
        !rendered.contains("opaque-test-credential"),
        "operator result leaked credential material: {rendered}"
    );
    let workspace_text = workspace.display().to_string();
    for entry in &result.evidence {
        assert!(
            !entry.contains("opaque-test-credential")
                && !entry.contains(&workspace_text)
                && !entry.contains("/private/")
                && !entry.contains("/var/"),
            "operator evidence is not bounded/redacted: {entry}"
        );
    }
    assert!(!result.summary.contains("opaque-test-credential"));
}

fn durable_ids(store: &OrchStore, work_id: &str) -> (Vec<String>, Vec<String>, usize) {
    let mut intents = store
        .list_managed_intents()
        .unwrap()
        .into_iter()
        .filter(|intent| intent.work_id == work_id)
        .map(|intent| intent.intent_id)
        .collect::<Vec<_>>();
    intents.sort();
    let mut attempts = store
        .list_work_attempts(Some(work_id))
        .unwrap()
        .into_iter()
        .map(|attempt| attempt.attempt_id)
        .collect::<Vec<_>>();
    attempts.sort();
    let runs = store.list_runs().unwrap().len();
    (intents, attempts, runs)
}

async fn drive_until_review_gate(orch: &OrchestrationService, work_id: &str) -> WorkItem {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        orch.drive_native_executor_once().await;
        let current = orch.store().load_work_item(work_id).unwrap().unwrap();
        if matches!(
            current.state,
            WorkState::AwaitingApproval
                | WorkState::Review
                | WorkState::Failed
                | WorkState::Succeeded
        ) {
            return current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "managed Grok dogfood did not reach a terminal operator state"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
}

async fn materialize_authorized_step(
    orch: &OrchestrationService,
    lane_id: Uuid,
    workspace: &Path,
    agent_id: &str,
    request_prefix: &str,
    step_id: &str,
    objective: &str,
) -> (String, String, u64) {
    let created = orch
        .create_manager_plan(
            &auth(),
            &format!("{request_prefix}-create"),
            lane_id,
            workspace,
            agent_id.to_string(),
            objective.to_string(),
            vec![isolated_review_step(agent_id, step_id, objective)],
            1,
            1,
            false,
        )
        .await
        .unwrap_or_else(|error| panic!("create manager plan: {error}"));
    let replay = orch
        .create_manager_plan(
            &auth(),
            &format!("{request_prefix}-create"),
            lane_id,
            workspace,
            agent_id.to_string(),
            objective.to_string(),
            vec![isolated_review_step(agent_id, step_id, objective)],
            1,
            1,
            false,
        )
        .await
        .unwrap_or_else(|error| panic!("replay manager plan: {error}"));
    assert_eq!(created["plan"]["planId"], replay["plan"]["planId"]);
    assert_eq!(created["plan"]["revision"], replay["plan"]["revision"]);
    assert_eq!(created["plan"]["sessionId"], lane_id.to_string());
    let plan_id = created["plan"]["planId"]
        .as_str()
        .expect("plan id")
        .to_string();
    let revision = created["plan"]["revision"].as_u64().expect("plan revision");

    let advanced = orch
        .advance_manager_plan(
            &auth(),
            &format!("{request_prefix}-advance"),
            lane_id,
            workspace,
            &plan_id,
            Some(revision),
        )
        .await
        .unwrap_or_else(|error| panic!("advance manager plan: {error}"));
    let replay_advance = orch
        .advance_manager_plan(
            &auth(),
            &format!("{request_prefix}-advance"),
            lane_id,
            workspace,
            &plan_id,
            Some(revision),
        )
        .await
        .unwrap_or_else(|error| panic!("replay advance: {error}"));
    assert_eq!(
        advanced["createdWork"][0]["workId"],
        replay_advance["createdWork"][0]["workId"]
    );
    let stale = orch
        .advance_manager_plan(
            &auth(),
            &format!("{request_prefix}-stale"),
            lane_id,
            workspace,
            &plan_id,
            Some(revision),
        )
        .await;
    assert!(stale.is_err(), "stale plan revision must fail closed");

    let work_id = advanced["createdWork"][0]["workId"]
        .as_str()
        .expect("work id")
        .to_string();
    let work = orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(work.session_id, lane_id);
    assert_eq!(
        dunce::canonicalize(Path::new(&work.workspace)).unwrap(),
        dunce::canonicalize(workspace).unwrap()
    );
    assert_eq!(
        work.source_manager_plan_id.as_deref(),
        Some(plan_id.as_str())
    );
    assert_eq!(work.source_manager_step_id.as_deref(), Some(step_id));
    assert_eq!(work.assigned_agent_id.as_deref(), Some(agent_id));
    assert_ne!(work.state, WorkState::Succeeded);

    let authorized = orch
        .authorize_work_execution(
            &auth(),
            &format!("{request_prefix}-authorize"),
            lane_id,
            workspace,
            &work_id,
            format!("authorize {request_prefix}"),
            Some(work.revision),
        )
        .await
        .unwrap_or_else(|error| panic!("authorize execution: {error}"));
    let replay_auth = orch
        .authorize_work_execution(
            &auth(),
            &format!("{request_prefix}-authorize"),
            lane_id,
            workspace,
            &work_id,
            format!("authorize {request_prefix}"),
            Some(work.revision),
        )
        .await
        .unwrap_or_else(|error| panic!("replay authorize: {error}"));
    assert_eq!(
        authorized["decision"]["decisionId"],
        replay_auth["decision"]["decisionId"]
    );
    (
        plan_id,
        work_id,
        authorized["work"]["revision"].as_u64().unwrap(),
    )
}

fn grok_executor_config(
    fake_grok: &Path,
    workspace: &Path,
    isolate_parent: &Path,
    identity: &GrokBuildGitIdentity,
    lease_id: &str,
) -> ManagedGrokExecutorConfig {
    ManagedGrokExecutorConfig {
        executable: fake_grok.to_path_buf(),
        git_executable: PathBuf::from("/usr/bin/git"),
        cwd: workspace.to_path_buf(),
        isolate_parent: isolate_parent.to_path_buf(),
        repository_id: identity.repository_id.clone(),
        base_ref: identity.git_ref.clone(),
        identity: identity.clone(),
        credential_lease_id: lease_id.into(),
    }
}

#[cfg(unix)]
async fn run_profile(profile: ManagedExecutionBudgetProfile) {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");

    let workspace = tempdir().unwrap();
    let identity = initialize_repo(workspace.path());
    let isolate_parent = tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(isolate_parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let fake_dir = tempdir().unwrap();
    let fake_grok = fake_dir.path().join("grok");
    install_fake_grok(&fake_grok);
    let lease_path = fake_dir.path().join("lease.json");
    fs::write(&lease_path, b"opaque-test-credential\n").unwrap();
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600)).unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire GrokPtah instance lock");
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "managed-grok-dogfood-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    orch.configure_managed_grok_executor(
        grok_executor_config(
            &fake_grok,
            workspace.path(),
            isolate_parent.path(),
            &identity,
            "dogfood-credential-lease",
        ),
        Arc::new(FileLeaseResolver {
            path: lease_path.clone(),
        }),
    )
    .unwrap();
    orch.store()
        .revise_agent_spec(&agent.agent_id, "operator", |spec| {
            spec.managed_execution = grok_policy(profile);
            spec.authority.computer_use_allowed = false;
            spec.authority.bypass_permissions = false;
            Ok(())
        })
        .unwrap();

    // IsolatedReview's source identity gate requires a clean tree, so the
    // separately authorized discard must run before a Clean promotion.
    set_fake_behavior(fake_dir.path(), "discard");
    let (_discard_plan, discard_work_id, _) = materialize_authorized_step(
        &orch,
        lane.id,
        workspace.path(),
        &agent.agent_id,
        "discard",
        "discard-step",
        "Propose a disposable DOGFOOD.txt mutation and leave it unpromoted.",
    )
    .await;
    let discarded = drive_until_review_gate(&orch, &discard_work_id).await;
    assert_eq!(
        discarded.state,
        WorkState::Review,
        "not_complete isolated mutation must stay advisory Review: {:?}",
        discarded.result
    );
    assert_ne!(discarded.state, WorkState::Succeeded);
    assert_redacted_operator_result(&discarded, workspace.path());
    let discard_result = discarded.result.as_ref().unwrap();
    assert!(discard_result.failure.is_none());
    assert!(!discard_result
        .evidence
        .iter()
        .any(|entry| entry.starts_with("changed_path:")));
    assert_eq!(
        fs::read_to_string(workspace.path().join("DOGFOOD.txt")).unwrap(),
        "before\n"
    );
    assert!(git(
        workspace.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"]
    )
    .is_empty());
    assert!(git(workspace.path(), &["remote"]).is_empty());

    set_fake_behavior(fake_dir.path(), "complete");
    let (_review_plan, review_work_id, _) = materialize_authorized_step(
        &orch,
        lane.id,
        workspace.path(),
        &agent.agent_id,
        "review",
        "dogfood-step",
        "Replace DOGFOOD.txt with the bounded managed-executor fixture.",
    )
    .await;
    let reviewed = drive_until_review_gate(&orch, &review_work_id).await;
    assert_eq!(
        reviewed.state,
        WorkState::AwaitingApproval,
        "managed executor finalized unexpectedly: {:?}",
        reviewed.result
    );
    assert_ne!(reviewed.state, WorkState::Succeeded);
    assert_redacted_operator_result(&reviewed, workspace.path());
    let result = reviewed.result.as_ref().unwrap();
    assert!(result.failure.is_none());
    assert!(result
        .evidence
        .iter()
        .any(|entry| entry == "changed_path:DOGFOOD.txt"));
    assert!(result
        .evidence
        .iter()
        .any(|entry| entry.starts_with("diff_digest:")));
    assert_eq!(
        fs::read_to_string(workspace.path().join("DOGFOOD.txt")).unwrap(),
        "after\n"
    );
    assert!(git(workspace.path(), &["remote"]).is_empty());

    let discard_ids = durable_ids(orch.store(), &discard_work_id);
    let review_ids = durable_ids(orch.store(), &review_work_id);
    assert_eq!(discard_ids.0.len(), 1);
    assert_eq!(discard_ids.1.len(), 1);
    assert_eq!(review_ids.0.len(), 1);
    assert_eq!(review_ids.1.len(), 1);
    assert_eq!(discard_ids.2, 0);
    assert_eq!(review_ids.2, 0);
    let intents = orch.store().list_managed_intents().unwrap();
    assert_eq!(intents.len(), 2);
    for intent in &intents {
        assert_eq!(intent.session_id, lane.id);
        assert_eq!(intent.state, ManagedIntentState::Finalized);
        let invocation = intent.grok.as_ref().unwrap();
        assert_ne!(invocation.prompt_hash, intent.input_hash);
        assert_eq!(invocation.profile, profile);
        assert!(invocation.host_execution_approved);
        assert_eq!(
            invocation.cli_permission_mode.as_str(),
            "host_mapped_bypass_permissions"
        );
    }
    let review_intent = intents
        .iter()
        .find(|candidate| candidate.work_id == review_work_id)
        .unwrap();
    assert_eq!(
        review_intent.grok.as_ref().unwrap().changed_paths,
        ["DOGFOOD.txt"]
    );
    assert!(review_intent.grok.as_ref().unwrap().diff_digest.is_some());
    let spec = orch
        .store()
        .load_agent(&agent.agent_id)
        .unwrap()
        .unwrap()
        .current_spec()
        .unwrap()
        .clone();
    assert!(!spec.authority.computer_use_allowed);
    assert!(!spec.authority.bypass_permissions);
    assert!(!spec.managed_execution.retry_eligible);
    assert!(fs::read_dir(isolate_parent.path())
        .unwrap()
        .next()
        .is_none());

    let lane_id = lane.id;
    let agent_id = agent.agent_id.clone();
    orch.stop_background_tasks().await;
    let shutdown = host.shutdown().await;
    assert!(
        shutdown.is_clean(),
        "host shutdown: {}",
        shutdown.operator_summary()
    );
    drop(orch);
    drop(host);

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("reacquire GrokPtah instance lock");
    host.start().unwrap();
    host.session_set_cwd(lane_id, workspace.path()).unwrap();
    let reopened_agent = host.ensure_session_agent(lane_id).unwrap();
    assert_eq!(reopened_agent.agent_id, agent_id);
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "managed-grok-dogfood-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    orch.configure_managed_grok_executor(
        grok_executor_config(
            &fake_grok,
            workspace.path(),
            isolate_parent.path(),
            &identity,
            "dogfood-credential-lease",
        ),
        Arc::new(FileLeaseResolver {
            path: lease_path.clone(),
        }),
    )
    .unwrap();
    for _ in 0..8 {
        orch.drive_native_executor_once().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }

    let discard_after = orch
        .store()
        .load_work_item(&discard_work_id)
        .unwrap()
        .unwrap();
    let review_after = orch
        .store()
        .load_work_item(&review_work_id)
        .unwrap()
        .unwrap();
    assert_eq!(discard_after.state, WorkState::Review);
    assert_eq!(review_after.state, WorkState::AwaitingApproval);
    assert_ne!(discard_after.state, WorkState::Succeeded);
    assert_ne!(review_after.state, WorkState::Succeeded);
    assert_eq!(durable_ids(orch.store(), &discard_work_id), discard_ids);
    assert_eq!(durable_ids(orch.store(), &review_work_id), review_ids);
    assert_eq!(orch.store().list_managed_intents().unwrap().len(), 2);
    assert!(orch
        .store()
        .list_managed_intents()
        .unwrap()
        .iter()
        .all(|intent| intent.state == ManagedIntentState::Finalized));
    assert_eq!(
        fs::read_to_string(workspace.path().join("DOGFOOD.txt")).unwrap(),
        "after\n"
    );
    assert!(fs::read_dir(isolate_parent.path())
        .unwrap()
        .next()
        .is_none());

    orch.stop_background_tasks().await;
    let shutdown = host.shutdown().await;
    assert!(
        shutdown.is_clean(),
        "reopened host shutdown: {}",
        shutdown.operator_summary()
    );
    drop(orch);
    drop(host);
    set_grokptah_home_override(None);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn economy_and_high_assurance_share_authority_and_finish_truthfully() {
    run_profile(ManagedExecutionBudgetProfile::Economy).await;
    run_profile(ManagedExecutionBudgetProfile::HighAssurance).await;
}

/// Explicit live-provider dogfood. The ignored gate is intentional: callers
/// must opt in with an authorized Grok executable and credential source. The
/// credential is copied into a disposable host lease before the adapter sees
/// it, and neither its path nor contents enter durable Work evidence.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit live Grok Build authorization"]
#[allow(clippy::await_holding_lock)]
async fn live_grok_build_dogfood_runs_both_profiles_under_one_authority() {
    run_live_profile(ManagedExecutionBudgetProfile::Economy).await;
    run_live_profile(ManagedExecutionBudgetProfile::HighAssurance).await;
}

#[cfg(unix)]
async fn run_live_profile(profile: ManagedExecutionBudgetProfile) {
    use std::os::unix::fs::PermissionsExt;

    let executable = PathBuf::from(
        std::env::var_os("GROKPTAH_LIVE_GROK")
            .expect("GROKPTAH_LIVE_GROK must name the authorized Grok executable"),
    );
    let credential_source = PathBuf::from(
        std::env::var_os("GROKPTAH_LIVE_GROK_AUTH")
            .expect("GROKPTAH_LIVE_GROK_AUTH must name the authorized credential file"),
    );
    let candidate_head = std::env::var("GROKPTAH_LIVE_CANDIDATE_HEAD")
        .expect("GROKPTAH_LIVE_CANDIDATE_HEAD must bind the qualification source");
    let evidence_dir = PathBuf::from(
        std::env::var_os("GROKPTAH_LIVE_EVIDENCE_DIR")
            .expect("GROKPTAH_LIVE_EVIDENCE_DIR must name the secret-free evidence directory"),
    );

    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");

    let workspace = tempdir().unwrap();
    let identity = initialize_repo(workspace.path());
    let isolate_parent = tempdir().unwrap();
    fs::set_permissions(isolate_parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let lease_parent = tempdir().unwrap();
    let lease_path = lease_parent.path().join("lease.json");
    fs::copy(&credential_source, &lease_path).expect("copy disposable Grok credential lease");
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600)).unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire GrokPtah instance lock");
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "managed-grok-live-dogfood-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable,
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: workspace.path().to_path_buf(),
            isolate_parent: isolate_parent.path().to_path_buf(),
            repository_id: identity.repository_id.clone(),
            base_ref: identity.git_ref.clone(),
            identity,
            credential_lease_id: "dogfood-credential-lease".into(),
        },
        Arc::new(FileLeaseResolver {
            path: lease_path.clone(),
        }),
    )
    .unwrap();
    orch.store()
        .revise_agent_spec(&agent.agent_id, "operator", |spec| {
            spec.managed_execution = grok_policy(profile);
            spec.authority.computer_use_allowed = false;
            spec.authority.bypass_permissions = false;
            Ok(())
        })
        .unwrap();

    let policy = WorkPolicy {
        retry: WorkRetryPolicy {
            max_attempts: 1,
            retry_failed: false,
            retry_expired: false,
            backoff_ms: 0,
        },
        allowed_files: vec!["DOGFOOD.txt".into()],
        ..WorkPolicy::default()
    };
    let mut work = WorkItem::new(
        "isolated-review",
        "Open DOGFOOD.txt and replace its exact contents `before\\n` with `after\\n`. Edit no other file. Inspect the final diff and report truthfully.",
        lane.id,
        workspace.path().display().to_string(),
        "operator",
        policy,
    )
    .unwrap();
    work.assigned_agent_id = Some(agent.agent_id.clone());
    work.assignment_status = AssignmentStatus::Accepted;
    work.source_manager_plan_id = Some("self-host-live-plan".into());
    work.source_manager_step_id = Some(format!("{}-dogfood", profile.as_str()));
    orch.store().save_work_item(&work).unwrap();
    let (work, _) = orch
        .store()
        .authorize_work_execution(
            &work.work_id,
            "operator",
            None,
            "authorize one bounded live GrokPtah self-host attempt",
            Some(work.revision),
            Utc::now(),
        )
        .unwrap();

    let deadline = tokio::time::Instant::now()
        + tokio::time::Duration::from_millis(profile.limits().max_duration_ms + 60_000);
    let final_work = loop {
        orch.drive_native_executor_once().await;
        let current = orch.store().load_work_item(&work.work_id).unwrap().unwrap();
        if matches!(
            current.state,
            WorkState::AwaitingApproval | WorkState::Review
        ) {
            break current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "live managed Grok dogfood exceeded its bounded profile deadline"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    };

    assert_eq!(
        final_work.state,
        WorkState::AwaitingApproval,
        "live managed executor did not return an advisory for approval: {:?}",
        final_work.result
    );
    let result = final_work.result.as_ref().expect("live advisory result");
    assert!(result.verification.is_none());
    assert!(result.failure.is_none());
    assert!(result
        .evidence
        .iter()
        .any(|entry| entry == "changed_path:DOGFOOD.txt"));
    assert_eq!(
        fs::read_to_string(workspace.path().join("DOGFOOD.txt")).unwrap(),
        "after\n"
    );
    assert!(git(workspace.path(), &["remote"]).is_empty());

    let intents = orch.store().list_managed_intents().unwrap();
    let intent = intents
        .iter()
        .find(|candidate| candidate.work_id == work.work_id)
        .unwrap();
    assert_eq!(intent.state, ManagedIntentState::Finalized);
    let invocation = intent.grok.as_ref().unwrap();
    assert_eq!(invocation.profile, profile);
    assert_eq!(invocation.changed_paths, ["DOGFOOD.txt"]);
    assert!(invocation.diff_digest.is_some());
    assert_ne!(invocation.prompt_hash, intent.input_hash);

    fs::create_dir_all(&evidence_dir).unwrap();
    let evidence = serde_json::json!({
        "candidateHead": candidate_head,
        "profile": profile.as_str(),
        "workId": final_work.work_id,
        "terminalState": format!("{:?}", final_work.state),
        "verificationPresent": result.verification.is_some(),
        "failurePresent": result.failure.is_some(),
        "evidence": result.evidence,
        "invocation": {
            "requestId": invocation.request_id,
            "promptHash": invocation.prompt_hash,
            "cliPermissionMode": invocation.cli_permission_mode.as_str(),
            "hostExecutionApproved": invocation.host_execution_approved,
            "finalHeadSha": invocation.final_head_sha,
            "finalRef": invocation.final_ref,
            "finalState": invocation.final_state,
            "verdict": invocation.verdict,
            "changedPaths": invocation.changed_paths,
            "diffDigest": invocation.diff_digest,
        },
        "authority": {
            "maxAttempts": 1,
            "retryFailed": false,
            "retryExpired": false,
            "computerUseAllowed": false,
            "bypassPermissions": false,
        },
    });
    fs::write(
        evidence_dir.join(format!("{}.json", profile.as_str())),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();

    if lease_path.exists() {
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&lease_path)
            .and_then(|file| file.sync_all())
            .unwrap();
        fs::remove_file(&lease_path).unwrap();
    }
    assert!(fs::read_dir(isolate_parent.path())
        .unwrap()
        .next()
        .is_none());
    orch.stop_background_tasks().await;
    let shutdown = host.shutdown().await;
    assert!(
        shutdown.is_clean(),
        "host shutdown: {}",
        shutdown.operator_summary()
    );
    drop(orch);
    drop(host);
    set_grokptah_home_override(None);
}

#[cfg(unix)]
const FAKE_GROK: &str = r#"#!/bin/sh
if [ "$1" = "inspect" ] && [ "$2" = "--json" ]; then
  printf '{"grokVersion":"1.0.5","channel":"stable","cwd":"%s","projectRoot":"%s","projectTrusted":true,"projectInstructions":[],"permissions":{"loaded":0,"managedSettingsActive":false,"managedSettingsExists":false,"managedSettingsPath":"/managed/settings","marketplaceAllowlist":[],"mcpServerAllowlist":[],"skipped":[],"sources":[]},"loginPolicy":{"apiKeyAuthDisabled":false,"disableApiKeyAuth":null,"forceLoginTeamUuid":null},"hooks":[],"skills":[],"agents":[],"plugins":[],"marketplaces":[],"mcpServers":[],"lspServers":[],"configSources":{"layers":[{"path":"%s/config.toml","role":"user"}]},"externalCompat":{"cells":[{"enabled":false,"source":"config","surface":"skills","vendor":"cursor"},{"enabled":false,"source":"config","surface":"rules","vendor":"cursor"},{"enabled":false,"source":"config","surface":"agents","vendor":"cursor"},{"enabled":false,"source":"config","surface":"mcps","vendor":"cursor"},{"enabled":false,"source":"config","surface":"hooks","vendor":"cursor"},{"enabled":false,"source":"config","surface":"sessions","vendor":"cursor"},{"enabled":false,"source":"config","surface":"skills","vendor":"claude"},{"enabled":false,"source":"config","surface":"rules","vendor":"claude"},{"enabled":false,"source":"config","surface":"agents","vendor":"claude"},{"enabled":false,"source":"config","surface":"mcps","vendor":"claude"},{"enabled":false,"source":"config","surface":"hooks","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"codex"}],"remoteSettingsLoaded":false}}\n' "$PWD" "$PWD" "$GROK_HOME"
  exit 0
fi
session_id=''
prompt_file=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--session-id' ]; then session_id="$arg"; fi
  if [ "$previous" = '--prompt-file' ]; then prompt_file="$arg"; fi
  previous="$arg"
done
[ "$HOME" = "$GROK_HOME" ] || exit 71
grep -q -- 'Exact mutable-file allowlist:' "$prompt_file" || exit 72
grep -q -- '- DOGFOOD.txt' "$prompt_file" || exit 73
grep -q -- 'GROK_BUILD_VERDICT=not_complete' "$prompt_file" || exit 74
grep -Eq -- 'Budget profile: (economy|high_assurance)' "$prompt_file" || exit 75
grep -q -- 'Do not commit, push, merge, fetch' "$prompt_file" || exit 76
if grep -q -- 'opaque-test-credential' "$prompt_file"; then exit 77; fi
behavior=complete
if [ -f "$(dirname "$0")/behavior" ]; then
  behavior=$(tr -d '\n' < "$(dirname "$0")/behavior")
fi
mkdir -p "$GROK_HOME/sessions/workspace/$session_id"
if [ "$behavior" = "discard" ]; then
  printf 'discarded\n' > DOGFOOD.txt
  printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded managed dogfood discard\\nGROK_BUILD_VERDICT=not_complete"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  printf '{"text":"bounded managed dogfood discard\\nGROK_BUILD_VERDICT=not_complete","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
  exit 0
fi
printf 'after\n' > DOGFOOD.txt
printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded managed dogfood\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"text":"bounded managed dogfood\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
"#;
