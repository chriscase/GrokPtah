//! End-to-end service coverage for the managed Grok Build executor.
//!
//! The child is a deterministic fake CLI. Live provider qualification is a
//! separate, explicit gate, but this test exercises the real durable Work,
//! authorization, dispatch, supervision, adapter, mutation-evidence, and
//! finalization path.

mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    AssignmentStatus, ManagedExecutionBudgetProfile, ManagedExecutionPolicy, ManagedExecutorKind,
    ManagedGrokExecutorConfig, ManagedIntentState, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, WorkItem, WorkPolicy, WorkRetryPolicy, WorkState,
    WorkspaceAllowlist,
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
        ManagedGrokExecutorConfig {
            executable: fake_grok,
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
        "Replace DOGFOOD.txt with the bounded managed-executor fixture.",
        lane.id,
        workspace.path().display().to_string(),
        "operator",
        policy,
    )
    .unwrap();
    work.assigned_agent_id = Some(agent.agent_id.clone());
    work.assignment_status = AssignmentStatus::Accepted;
    work.source_manager_plan_id = Some("self-host-plan".into());
    work.source_manager_step_id = Some("dogfood-step".into());
    orch.store().save_work_item(&work).unwrap();
    let (work, _) = orch
        .store()
        .authorize_work_execution(
            &work.work_id,
            "operator",
            None,
            "authorize one bounded self-host dogfood attempt",
            Some(work.revision),
            Utc::now(),
        )
        .unwrap();

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        orch.drive_native_executor_once().await;
        let current = orch.store().load_work_item(&work.work_id).unwrap().unwrap();
        if matches!(
            current.state,
            WorkState::AwaitingApproval | WorkState::Review
        ) {
            assert_eq!(
                current.state,
                WorkState::AwaitingApproval,
                "managed executor finalized unexpectedly: {:?}",
                current.result
            );
            let result = current.result.expect("bounded advisory result");
            assert!(result.verification.is_none());
            assert!(result.failure.is_none());
            assert!(result
                .evidence
                .iter()
                .any(|entry| entry == "changed_path:DOGFOOD.txt"));
            assert!(result
                .evidence
                .iter()
                .any(|entry| entry.starts_with("diff_digest:")));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "managed Grok dogfood did not reach a terminal operator state"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }

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
    assert_ne!(invocation.prompt_hash, intent.input_hash);
    assert_eq!(invocation.changed_paths, ["DOGFOOD.txt"]);
    assert!(invocation.diff_digest.is_some());

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
printf 'after\n' > DOGFOOD.txt
mkdir -p "$GROK_HOME/sessions/workspace/$session_id"
printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded managed dogfood\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"text":"bounded managed dogfood\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
"#;
