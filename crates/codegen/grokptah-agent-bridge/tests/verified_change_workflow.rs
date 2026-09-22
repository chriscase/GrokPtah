//! Production managed-work journey for one bounded multi-file repair.
//!
//! The child is a deterministic fake CLI. Required checks run on the host
//! against the retained candidate, outside the worker's writable tree.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use grokptah_agent_bridge::orchestration::{
    AuthContext, ManagedExecutionBudgetProfile, ManagedGrokExecutorConfig, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, VerifiedChangeRequest, WorkState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    execute_required_checks, set_grokptah_home_override, AgentHost, CredentialLeaseHandle,
    CredentialLeaseResolver, GrokBuildAdapterError, HostConfig, HostRuntime, RequiredCheckCwd,
    RequiredCheckSpec, SessionKind,
};
use grokptah_agent_sdk::GrokBuildGitIdentity;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const LEDGER_BEFORE: &str =
    "pub fn balance(cents: &[i32]) -> i32 {\n    cents.iter().sum::<i32>().saturating_add(1)\n}\n";
const REPORT_BEFORE: &str = "pub fn render(cents: &[i32]) -> String {\n    format!(\"balance:{}\", crate::ledger::balance(cents))\n}\n";
const LEDGER_AFTER: &str = "pub fn balance(cents: &[i32]) -> i32 {\n    cents.iter().sum()\n}\n";
const REPORT_AFTER: &str = "pub fn render(cents: &[i32]) -> String {\n    // COORDINATED_REPORT=1\n    format!(\"balance:{}\", crate::ledger::balance(cents))\n}\n";

struct FileLeaseResolver {
    path: PathBuf,
}

impl CredentialLeaseResolver for FileLeaseResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        if lease_id != "verified-change-lease" {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        Ok(CredentialLeaseHandle::from_host_path(self.path.clone()))
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        if lease_id != "verified-change-lease" {
            return Err(GrokBuildAdapterError::CredentialRevocation);
        }
        if self.path.exists() {
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
        .expect("git");
    assert!(
        output.status.success(),
        "{args:?} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_repo(repo: &Path) -> GrokBuildGitIdentity {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/ledger.rs"), LEDGER_BEFORE).unwrap();
    fs::write(repo.join("src/report.rs"), REPORT_BEFORE).unwrap();
    git(repo, &["init", "-b", "topic"]);
    git(repo, &["config", "user.name", "GrokPtah test"]);
    git(repo, &["config", "user.email", "test@grokptah.invalid"]);
    git(repo, &["add", "src/ledger.rs", "src/report.rs"]);
    git(repo, &["commit", "-m", "buggy pair"]);
    let head = git(repo, &["rev-parse", "HEAD"]);
    GrokBuildGitIdentity {
        repository_id: "repo-verified-change".into(),
        git_ref: "refs/heads/topic".into(),
        base_sha: head.clone(),
        head_sha: head,
    }
}

fn install_oracle(dir: &Path) -> RequiredCheckSpec {
    let script = dir.join("balance-regression.sh");
    fs::write(
        &script,
        "#!/bin/sh\nledger=\"$CANDIDATE_ROOT/src/ledger.rs\"\nreport=\"$CANDIDATE_ROOT/src/report.rs\"\ngrep -q 'saturating_add(1)' \"$ledger\" && exit 1\ngrep -q 'iter().sum()' \"$ledger\" || exit 1\ngrep -q 'ledger::balance' \"$report\" || exit 1\ngrep -q 'COORDINATED_REPORT=1' \"$report\" || exit 1\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    RequiredCheckSpec {
        check_id: "balance-regression".into(),
        executable: script.display().to_string(),
        args: Vec::new(),
        cwd: RequiredCheckCwd::Oracle,
        env: Vec::new(),
        timeout_ms: 2000,
        max_output_bytes: 1024,
    }
}

fn install_fake(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, FAKE_GROK).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn auth() -> AuthContext {
    AuthContext {
        token_id: "operator".into(),
        owner_id: "primary".into(),
    }
}

struct Harness {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    isolate: tempfile::TempDir,
    oracle: tempfile::TempDir,
    fake_dir: tempfile::TempDir,
    identity: GrokBuildGitIdentity,
    check: RequiredCheckSpec,
    host: HostRuntime,
    lane: Uuid,
    agent_id: String,
    orch: Arc<OrchestrationService>,
    _env: ProcessEnvGuard,
}

impl Harness {
    fn open(profile: ManagedExecutionBudgetProfile) -> Self {
        let mut env = ProcessEnvGuard::new();
        env.set("GROKPTAH_AGENT_OFFLINE", "1");
        env.set("GITHUB_TOKEN", "opaque-test-credential");
        env.set("HOME", "/operator/home");
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let workspace = tempdir().unwrap();
        let identity = init_repo(workspace.path());
        let isolate = tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(isolate.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let oracle = tempdir().unwrap();
        let check = install_oracle(oracle.path());
        let fake_dir = tempdir().unwrap();
        let fake = fake_dir.path().join("grok");
        install_fake(&fake);
        let lease = fake_dir.path().join("lease.json");
        fs::write(&lease, b"opaque-test-credential\n").unwrap();
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o600)).unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        })
        .expect("host");
        host.start().unwrap();
        let lane = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(lane.id, workspace.path()).unwrap();
        let agent = host.ensure_session_agent(lane.id).unwrap();
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            OrchStore::open(home.path().join("orchestration")).unwrap(),
            OrchestrationConfig {
                bearer_token: "verified-change-token".into(),
                allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
                max_concurrent_runs: 1,
                bounds: RunBounds::default(),
            },
        );
        orch.configure_managed_grok_executor(
            ManagedGrokExecutorConfig {
                executable: fake,
                git_executable: PathBuf::from("/usr/bin/git"),
                cwd: workspace.path().to_path_buf(),
                isolate_parent: isolate.path().to_path_buf(),
                repository_id: identity.repository_id.clone(),
                base_ref: identity.git_ref.clone(),
                identity: identity.clone(),
                credential_lease_id: "verified-change-lease".into(),
            },
            Arc::new(FileLeaseResolver { path: lease }),
        )
        .unwrap();
        let _ = profile;
        Self {
            home,
            workspace,
            isolate,
            oracle,
            fake_dir,
            identity,
            check,
            host,
            lane: lane.id,
            agent_id: agent.agent_id,
            orch,
            _env: env,
        }
    }

    fn request(&self, request_id: &str, mode: &str, platform: &str) -> VerifiedChangeRequest {
        VerifiedChangeRequest {
            request_id: request_id.into(),
            session_id: self.lane,
            workspace: self.workspace.path().to_path_buf(),
            agent_id: self.agent_id.clone(),
            objective: "Repair the balance so the report and ledger agree without the extra cent."
                .into(),
            allowed_files: vec!["src/ledger.rs".into(), "src/report.rs".into()],
            required_checks: vec![self.check.clone()],
            oracle_root: self.oracle.path().to_path_buf(),
            budget_profile: ManagedExecutionBudgetProfile::Economy,
            mutation_mode: mode.into(),
            platform: platform.into(),
            execution_host: "service".into(),
        }
    }

    fn set_behavior(&self, behavior: &str) {
        fs::write(self.fake_dir.path().join("behavior"), behavior).unwrap();
    }

    async fn close(self) {
        self.orch.stop_background_tasks().await;
        let _ = self.host.shutdown().await;
        let _ = self.home.path();
        let _ = self.isolate.path();
        set_grokptah_home_override(None);
    }

    fn source_pair(&self) -> (String, String) {
        (
            fs::read_to_string(self.workspace.path().join("src/ledger.rs")).unwrap(),
            fs::read_to_string(self.workspace.path().join("src/report.rs")).unwrap(),
        )
    }
}

fn assert_secret_free(value: &serde_json::Value) {
    let rendered = value.to_string();
    assert!(!rendered.contains("opaque-test-credential"), "{rendered}");
    assert!(!rendered.contains("GITHUB_TOKEN"), "{rendered}");
    assert!(!rendered.contains("/operator/home"), "{rendered}");
    assert!(!rendered.contains("verified-change-token"), "{rendered}");
}

async fn settle(orch: &OrchestrationService, work_id: &str) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(15);
    loop {
        orch.drive_native_executor_once().await;
        let work = orch.store().load_work_item(work_id).unwrap().unwrap();
        if matches!(
            work.state,
            WorkState::AwaitingApproval
                | WorkState::Review
                | WorkState::Failed
                | WorkState::Cancelled
                | WorkState::Succeeded
        ) {
            return serde_json::to_value(&work).unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "work did not settle: {:?}",
            work.state
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_file_repair_is_red_then_green_and_apply_is_separate() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let before = execute_required_checks(
        std::slice::from_ref(&harness.check),
        harness.workspace.path(),
        harness.oracle.path(),
    )
    .unwrap();
    assert_eq!(before[0].outcome, "failed");
    let oracle_bytes = fs::read(harness.oracle.path().join("balance-regression.sh")).unwrap();

    let blocked = harness
        .orch
        .prepare_verified_change(
            &auth(),
            &harness.request("linux", "isolated_review", "linux"),
        )
        .unwrap();
    assert_eq!(blocked["readiness"]["ready"], false);
    assert_eq!(blocked["readiness"]["workersDispatched"], 0);
    assert_eq!(blocked["readiness"]["providerInvocations"], 0);
    assert!(blocked["readiness"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("Non-macOS")));
    let work_before = harness.orch.store().list_work_items().unwrap_or_default();
    let start_blocked = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("linux", "isolated_review", "linux"),
        )
        .await;
    assert!(start_blocked.is_err());
    assert_eq!(
        harness.orch.store().list_managed_intents().unwrap().len(),
        0
    );
    let _ = work_before;

    let readonly = harness
        .orch
        .prepare_verified_change(&auth(), &harness.request("ro", "read_only", "macos"))
        .unwrap();
    assert_eq!(readonly["readiness"]["ready"], false);
    assert!(readonly["readiness"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("Read-only")));

    let prepared = harness
        .orch
        .prepare_verified_change(
            &auth(),
            &harness.request("repair", "isolated_review", "macos"),
        )
        .unwrap();
    assert_eq!(prepared["readiness"]["ready"], true);
    assert_eq!(prepared["readiness"]["workersDispatched"], 0);
    assert_eq!(prepared["cliContract"], "inspect-json-v1");
    assert_eq!(prepared["cliVersion"], "1.0.5");
    assert_ne!(prepared["modelSelectionKey"], prepared["cliVersion"]);
    assert_eq!(prepared["repositoryId"], "repo-verified-change");
    assert_eq!(prepared["sourceRevision"], harness.identity.head_sha);
    assert_eq!(prepared["agentId"], harness.agent_id);
    assert_eq!(prepared["executionHost"], "service");
    assert_eq!(prepared["executor"], "grok_build_isolated_review");
    assert_eq!(prepared["allowedFiles"][0], "src/ledger.rs");
    assert_eq!(prepared["allowedFiles"][1], "src/report.rs");
    assert_eq!(prepared["approvalRequired"], true);
    assert!(prepared["limits"]["maxRounds"].as_u64().unwrap() > 0);
    assert_secret_free(&prepared);
    let inspect_log =
        fs::read_to_string(harness.fake_dir.path().join("grok.argv")).unwrap_or_default();
    assert!(inspect_log.contains("inspect --json"), "{inspect_log}");
    assert!(!inspect_log.contains("prompt-file"), "{inspect_log}");

    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("repair", "isolated_review", "macos"),
        )
        .await
        .unwrap_or_else(|error| panic!("start: {error}"));
    let work_id = started["workId"].as_str().unwrap().to_string();
    assert_eq!(started["attemptCount"], 1);
    assert_eq!(started["cliVersion"], "1.0.5");
    assert_eq!(started["cliContract"], "inspect-json-v1");
    assert_eq!(started["executionHost"], "service");
    assert_eq!(started["sourceRevision"], harness.identity.head_sha);
    assert_eq!(started["readiness"]["ready"], true);
    assert_eq!(started["readiness"]["workersDispatched"], 0);
    assert_eq!(started["readiness"]["providerInvocations"], 0);
    assert_secret_free(&started);
    let settled = settle(&harness.orch, &work_id).await;
    assert_eq!(
        settled["state"].as_str().unwrap_or(""),
        "awaiting_approval",
        "result={}",
        settled["result"]
    );
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    let status = harness
        .orch
        .verified_change_status(&auth(), harness.lane, harness.workspace.path(), &work_id)
        .unwrap();
    assert_eq!(status["phases"]["workerStopped"], true);
    assert_eq!(status["phases"]["changeProposed"], true);
    assert_eq!(status["phases"]["checksPassed"], true);
    assert_eq!(status["cliVersion"], "1.0.5");
    assert_eq!(status["executionHost"], "service");
    assert_eq!(status["sourceRevision"], harness.identity.head_sha);
    assert_eq!(status["readiness"]["workersDispatched"], 0);
    assert_eq!(status["phases"]["humanApproved"], false);
    assert_eq!(status["phases"]["applied"], false);
    assert_eq!(status["checkResults"][0]["outcome"], "passed");
    assert_eq!(status["changedPaths"].as_array().unwrap().len(), 2);
    assert_secret_free(&status);
    assert_eq!(
        fs::read(harness.oracle.path().join("balance-regression.sh")).unwrap(),
        oracle_bytes
    );
    assert_eq!(
        harness
            .orch
            .store()
            .list_work_attempts(Some(&work_id))
            .unwrap()
            .len(),
        1
    );

    let again = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("repair", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    assert_eq!(again["workId"], work_id);
    assert_eq!(
        harness
            .orch
            .store()
            .list_work_attempts(Some(&work_id))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        harness
            .orch
            .store()
            .list_managed_intents()
            .unwrap()
            .iter()
            .filter(|intent| intent.work_id == work_id)
            .count(),
        1
    );

    let revision = settled["revision"].as_u64();
    let digest = status["checkResults"][0]["specDigest"].as_str().unwrap();
    let _ = digest;
    let snapshot = harness
        .orch
        .store()
        .candidate_snapshot_dir(&work_id)
        .unwrap();
    let tampered = snapshot.join("src/ledger.rs");
    let saved = fs::read(&tampered).unwrap();
    fs::write(&tampered, b"tampered\n").unwrap();
    let invalid = harness
        .orch
        .verified_change_status(&auth(), harness.lane, harness.workspace.path(), &work_id)
        .unwrap();
    assert_eq!(invalid["phases"]["checksPassed"], false);
    assert!(invalid["safeAction"]
        .as_str()
        .unwrap()
        .contains("changed after verification"));
    let approve_stale = harness
        .orch
        .approve_work(
            &auth(),
            "stale-approve",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            Some("looks good".into()),
            revision,
        )
        .await;
    assert!(approve_stale.is_err());
    fs::write(&tampered, saved).unwrap();

    harness.set_behavior("repair");
    let second = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("apply", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let apply_id = second["workId"].as_str().unwrap().to_string();
    assert_ne!(apply_id, work_id);
    let _ = settle(&harness.orch, &apply_id).await;
    let apply_status = harness
        .orch
        .verified_change_status(&auth(), harness.lane, harness.workspace.path(), &apply_id)
        .unwrap();
    assert_eq!(apply_status["phases"]["checksPassed"], true);
    let apply_work = harness
        .orch
        .store()
        .load_work_item(&apply_id)
        .unwrap()
        .unwrap();
    let approved = harness
        .orch
        .approve_work(
            &auth(),
            "approve-apply",
            harness.lane,
            harness.workspace.path(),
            &apply_id,
            Some("approve the exact candidate".into()),
            Some(apply_work.revision),
        )
        .await
        .unwrap_or_else(|error| panic!("approve: {error}"));
    assert_eq!(approved["work"]["state"], "awaiting_approval");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    let digest = apply_work
        .result
        .unwrap()
        .candidate_verification
        .unwrap()
        .content_digest;
    let approved_work = harness
        .orch
        .store()
        .load_work_item(&apply_id)
        .unwrap()
        .unwrap();
    let applied = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-exact",
            harness.lane,
            harness.workspace.path(),
            &apply_id,
            &digest,
            Some(approved_work.revision),
        )
        .await
        .unwrap_or_else(|error| panic!("apply: {error}"));
    assert_eq!(applied["work"]["state"], "succeeded");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );
    let wrong = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-applied",
            harness.lane,
            harness.workspace.path(),
            &apply_id,
            &digest,
            None,
        )
        .await;
    assert!(wrong.is_err());
    assert_eq!(
        harness.source_pair(),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );
    assert!(git(harness.workspace.path(), &["remote"]).is_empty());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_scope_and_skipped_checks_cannot_verify() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("noop");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("noop", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_eq!(settled["state"], "review");
    assert_ne!(settled["candidateVerification"]["checksPassed"], true);
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );

    fs::write(
        harness.fake_dir.path().join("lease.json"),
        b"opaque-test-credential\n",
    )
    .unwrap();
    harness.set_behavior("escape");
    let escaped = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("escape", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let escape_id = escaped["workId"].as_str().unwrap().to_string();
    let escape_settled = settle(&harness.orch, &escape_id).await;
    assert_ne!(escape_settled["state"], "succeeded");
    assert_ne!(
        escape_settled["candidateVerification"]["checksPassed"],
        true
    );
    assert!(!harness.workspace.path().join("NOTES.txt").exists());

    fs::write(
        harness.fake_dir.path().join("lease.json"),
        b"opaque-test-credential\n",
    )
    .unwrap();
    harness.set_behavior("symlink");
    let linked = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("symlink", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let link_id = linked["workId"].as_str().unwrap().to_string();
    let link_settled = settle(&harness.orch, &link_id).await;
    assert_ne!(link_settled["state"], "succeeded");
    assert_ne!(link_settled["candidateVerification"]["checksPassed"], true);
    let ledger = fs::symlink_metadata(harness.workspace.path().join("src/ledger.rs")).unwrap();
    assert!(!ledger.file_type().is_symlink());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_and_restart_do_not_dispatch_or_apply() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("hold");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("hold", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let running = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    harness
        .orch
        .cancel_work(
            &auth(),
            "cancel-hold",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            "stop before promotion".into(),
            Some(running.revision),
        )
        .await
        .unwrap();
    fs::write(harness.fake_dir.path().join("release"), b"go\n").unwrap();
    let settled = settle(&harness.orch, &work_id).await;
    assert_eq!(settled["state"], "cancelled");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    let attempts = harness
        .orch
        .store()
        .list_work_attempts(Some(&work_id))
        .unwrap()
        .len();
    assert_eq!(attempts, 1);

    fs::write(
        harness.fake_dir.path().join("lease.json"),
        b"opaque-test-credential\n",
    )
    .unwrap();
    harness.set_behavior("repair");
    let review = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("restart", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let review_id = review["workId"].as_str().unwrap().to_string();
    let _ = settle(&harness.orch, &review_id).await;
    let before_attempts = harness
        .orch
        .store()
        .list_work_attempts(Some(&review_id))
        .unwrap()
        .len();
    let store_root = harness.orch.store().root().to_path_buf();
    let workspace = harness.workspace.path().to_path_buf();
    let lane = harness.lane;
    let agent_id = harness.agent_id.clone();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let head_sha = identity.head_sha.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    harness.orch.stop_background_tasks().await;
    harness.host.shutdown().await;
    drop(harness.orch);
    drop(harness.host);

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(&store_root).unwrap(),
        OrchestrationConfig {
            bearer_token: "verified-change-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.clone()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable: fake,
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: workspace.clone(),
            isolate_parent: isolate,
            repository_id: identity.repository_id.clone(),
            base_ref: identity.git_ref.clone(),
            identity,
            credential_lease_id: "verified-change-lease".into(),
        },
        Arc::new(FileLeaseResolver { path: lease }),
    )
    .unwrap();
    for _ in 0..4 {
        orch.drive_native_executor_once().await;
    }
    let reopened = orch.store().load_work_item(&review_id).unwrap().unwrap();
    assert_eq!(reopened.state, WorkState::AwaitingApproval);
    assert_eq!(
        orch.store()
            .list_work_attempts(Some(&review_id))
            .unwrap()
            .len(),
        before_attempts
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );
    let status = orch
        .verified_change_status(&auth(), lane, &workspace, &review_id)
        .unwrap();
    assert_eq!(status["phases"]["checksPassed"], true);
    assert_eq!(status["phases"]["applied"], false);
    assert_eq!(status["cliVersion"], "1.0.5");
    assert_eq!(status["executionHost"], "service");
    assert_eq!(status["sourceRevision"], head_sha);
    assert_eq!(status["readiness"]["workersDispatched"], 0);
    assert_eq!(status["attemptCount"], before_attempts);
    assert_secret_free(&status);
    let _ = agent_id;
    orch.stop_background_tasks().await;
    host.shutdown().await;
    set_grokptah_home_override(None);
}

const FAKE_GROK: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/grok.argv"
if [ "$1" = "inspect" ] && [ "$2" = "--json" ]; then
  printf '{"grokVersion":"1.0.5","channel":"stable","cwd":"%s","projectRoot":"%s","projectTrusted":true,"projectInstructions":[],"permissions":{"loaded":0,"managedSettingsActive":false,"managedSettingsExists":false,"managedSettingsPath":"/managed/settings","marketplaceAllowlist":[],"mcpServerAllowlist":[],"skipped":[],"sources":[]},"loginPolicy":{"apiKeyAuthDisabled":false,"disableApiKeyAuth":null,"forceLoginTeamUuid":null},"hooks":[],"skills":[],"agents":[],"plugins":[],"marketplaces":[],"mcpServers":[],"lspServers":[],"configSources":{"layers":[{"path":"%s/config.toml","role":"user"}]},"externalCompat":{"cells":[{"enabled":false,"source":"config","surface":"skills","vendor":"cursor"},{"enabled":false,"source":"config","surface":"rules","vendor":"cursor"},{"enabled":false,"source":"config","surface":"agents","vendor":"cursor"},{"enabled":false,"source":"config","surface":"mcps","vendor":"cursor"},{"enabled":false,"source":"config","surface":"hooks","vendor":"cursor"},{"enabled":false,"source":"config","surface":"sessions","vendor":"cursor"},{"enabled":false,"source":"config","surface":"skills","vendor":"claude"},{"enabled":false,"source":"config","surface":"rules","vendor":"claude"},{"enabled":false,"source":"config","surface":"agents","vendor":"claude"},{"enabled":false,"source":"config","surface":"mcps","vendor":"claude"},{"enabled":false,"source":"config","surface":"hooks","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"codex"}],"remoteSettingsLoaded":false}}\n' "$PWD" "$PWD" "$GROK_HOME"
  exit 0
fi
session_id=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--session-id' ]; then session_id="$arg"; fi
  previous="$arg"
done
[ "$HOME" = "$GROK_HOME" ] || exit 71
behavior=repair
if [ -f "$(dirname "$0")/behavior" ]; then
  behavior=$(tr -d '\n' < "$(dirname "$0")/behavior")
fi
if [ "$behavior" = "hold" ]; then
  while [ ! -f "$(dirname "$0")/release" ]; do
    sleep 0.05
  done
fi
mkdir -p "$GROK_HOME/sessions/workspace/$session_id"
if [ "$behavior" = "escape" ]; then
  printf 'outside\n' > NOTES.txt
fi
if [ "$behavior" = "symlink" ]; then
  rm -f src/ledger.rs
  ln -s /etc/passwd src/ledger.rs
fi
if [ "$behavior" = "repair" ] || [ "$behavior" = "hold" ] || [ "$behavior" = "escape" ]; then
  mkdir -p src
  printf '%s' 'pub fn balance(cents: &[i32]) -> i32 {
    cents.iter().sum()
}
' > src/ledger.rs
  printf '%s' 'pub fn render(cents: &[i32]) -> String {
    // COORDINATED_REPORT=1
    format!("balance:{}", crate::ledger::balance(cents))
}
' > src/report.rs
fi
printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"repaired the balance pair\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"text":"repaired the balance pair\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
"#;
