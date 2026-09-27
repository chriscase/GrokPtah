//! Production managed-work journey for one bounded multi-file repair.
//!
//! The child is a deterministic fake CLI. Required checks run on the host
//! against the retained candidate, outside the worker's writable tree.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use grokptah_agent_bridge::orchestration::{
    ApplyPhase, AuthContext, IdempotencyScope, ManagedExecutionBudgetProfile,
    ManagedGrokExecutorConfig, ManagedIntentState, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, VerifiedChangeRequest, WorkPolicy, WorkState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    derived_snapshot_fingerprint, directory_digest, execute_required_checks, file_digest,
    read_check_authority, recompute_candidate_apply_bundle, retained_candidate_diff_digest,
    set_grokptah_home_override, start_control_server, write_check_authority, AgentHost,
    CredentialLeaseHandle, CredentialLeaseResolver, GrokBuildAdapterError, HostConfig,
    HostLeaseAuthority, HostRuntime, RequiredCheckCwd, RequiredCheckSpec, SessionKind,
    ADMISSION_FAULT, APPLY_FAULT, BEFORE_CANDIDATE_BIND, CHECK_CONFINEMENT_EXECUTABLE,
    CLEANUP_FAULT, RECEIPT_TREE_SCANS, SKIP_VERIFIED_DRIVE,
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
const NOTE_AFTER: &str = "pub fn note() -> i32 { 1 }\n";

struct FileLeaseResolver {
    authority: HostLeaseAuthority,
}

impl CredentialLeaseResolver for FileLeaseResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        self.authority.resolve(lease_id)
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        self.authority.revoke(lease_id)
    }

    fn revokes_upstream(&self) -> bool {
        true
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

fn install_profile(store: &Path, executable: &str, oracle: &Path, revision: u64) {
    let executable_path = Path::new(executable);
    let body = serde_json::json!({
        "profileId": "balance-regression",
        "profileRevision": revision,
        "executable": executable,
        "executableDigest": file_digest(executable_path).expect("executable digest"),
        "args": [],
        "cwd": "oracle",
        "oracleRoot": oracle,
        "oracleDigest": directory_digest(oracle).expect("oracle digest"),
        "env": [],
        "timeoutMs": 2000,
        "maxOutputBytes": 1024,
        "network": "none",
        "maxOutputDirBytes": 65536,
    });
    let dir = store.join("check-profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("balance-regression.json"),
        serde_json::to_vec(&body).unwrap(),
    )
    .unwrap();
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
            Arc::new(FileLeaseResolver {
                authority: HostLeaseAuthority::new(fake_dir.path().to_path_buf()),
            }),
        )
        .unwrap();
        install_profile(orch.store().root(), &check.executable, oracle.path(), 1);
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

    fn request_files(&self, request_id: &str, files: Vec<String>) -> VerifiedChangeRequest {
        let mut request = self.request(request_id, "isolated_review", "macos");
        request.allowed_files = files;
        request
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
            check_profile_id: "balance-regression".into(),
            required_checks: Vec::new(),
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

struct LiveRuntime {
    host: HostRuntime,
    orch: Arc<OrchestrationService>,
}

async fn reopen_production_store(
    host: HostRuntime,
    orch: Arc<OrchestrationService>,
    workspace: &Path,
    fake: &Path,
    isolate: &Path,
    identity: &GrokBuildGitIdentity,
    lease: &Path,
) -> LiveRuntime {
    let store_root = orch.store().root().to_path_buf();
    let attempts_before = orch.store().list_work_attempts(None).unwrap().len();
    orch.stop_background_tasks().await;
    host.shutdown().await;
    drop(orch);
    drop(host);
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
            allowlist: WorkspaceAllowlist::new([workspace.to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable: fake.to_path_buf(),
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: workspace.to_path_buf(),
            isolate_parent: isolate.to_path_buf(),
            repository_id: identity.repository_id.clone(),
            base_ref: identity.git_ref.clone(),
            identity: identity.clone(),
            credential_lease_id: "verified-change-lease".into(),
        },
        Arc::new(FileLeaseResolver {
            authority: HostLeaseAuthority::new(lease.parent().unwrap().to_path_buf()),
        }),
    )
    .unwrap();
    for _ in 0..4 {
        orch.drive_native_executor_once().await;
    }
    assert_eq!(
        orch.store().list_work_attempts(None).unwrap().len(),
        attempts_before,
        "restart dispatched additional work attempts"
    );
    LiveRuntime { host, orch }
}

fn restore_lease(path: &Path) {
    fs::write(path, b"opaque-test-credential\n").unwrap();
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
    assert_eq!(blocked["readiness"]["platform"], "macos");
    assert_eq!(blocked["executionHost"], "service");
    assert_eq!(blocked["readiness"]["workersDispatched"], 0);
    assert_eq!(blocked["readiness"]["providerInvocations"], 0);
    assert!(blocked["readiness"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .all(|reason| !reason.as_str().unwrap_or("").contains("linux")));
    assert_eq!(
        harness.orch.store().list_managed_intents().unwrap().len(),
        0
    );

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
    assert_eq!(
        apply_status["phases"]["checksPassed"], true,
        "{apply_status}"
    );
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
    let settled_review = settle(&harness.orch, &review_id).await;
    assert_eq!(
        settled_review["state"], "awaiting_approval",
        "{settled_review}"
    );
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
        Arc::new(FileLeaseResolver {
            authority: HostLeaseAuthority::new(lease.parent().unwrap().to_path_buf()),
        }),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_at_admission_approval_and_apply_names_a_safe_action() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let behavior = harness.fake_dir.path().join("behavior");
    let lane = harness.lane;
    let admit_request = harness.request("admit-cut", "isolated_review", "macos");
    let approve_request = harness.request("approve-cut", "isolated_review", "macos");

    harness.set_behavior("hold");
    let admitted = harness
        .orch
        .start_verified_change(&auth(), &admit_request)
        .await
        .unwrap_or_else(|error| panic!("admit start: {error}"));
    let admit_id = admitted["workId"].as_str().unwrap().to_string();
    let running = harness
        .orch
        .store()
        .load_work_item(&admit_id)
        .unwrap()
        .unwrap();
    assert!(
        matches!(running.state, WorkState::Leased | WorkState::Running),
        "admission cut missed the in-flight state: {:?}",
        running.state
    );
    assert_eq!(
        harness
            .orch
            .store()
            .list_work_attempts(Some(&admit_id))
            .unwrap()
            .len(),
        1
    );
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .iter()
        .any(|intent| {
            intent.work_id == admit_id && intent.state == ManagedIntentState::Dispatching
        }));
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );

    let mut live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let uncertain = live
        .orch
        .store()
        .load_work_item(&admit_id)
        .unwrap()
        .unwrap();
    assert_eq!(uncertain.state, WorkState::Review);
    assert_eq!(
        uncertain
            .result
            .as_ref()
            .and_then(|result| result.failure.as_deref()),
        Some("grok_dispatch_uncertain_after_restart")
    );
    assert_eq!(
        live.orch
            .store()
            .list_work_attempts(Some(&admit_id))
            .unwrap()
            .len(),
        1
    );
    let admit_status = live
        .orch
        .verified_change_status(&auth(), lane, &workspace, &admit_id)
        .unwrap();
    assert_eq!(admit_status["phases"]["checksPassed"], false);
    assert_eq!(admit_status["phases"]["humanApproved"], false);
    assert_eq!(admit_status["phases"]["applied"], false);
    assert_eq!(admit_status["attemptCount"], 1);
    assert!(admit_status["safeAction"]
        .as_str()
        .unwrap()
        .contains("Do not dispatch another attempt"));
    assert!(admit_status["safeAction"]
        .as_str()
        .unwrap()
        .contains("verified success"));
    assert_secret_free(&admit_status);
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );
    let admit_again = live
        .orch
        .start_verified_change(&auth(), &admit_request)
        .await
        .unwrap_or_else(|error| panic!("admit reconnect: {error}"));
    assert_eq!(admit_again["workId"], admit_id);
    assert_eq!(admit_again["attemptCount"], 1);

    restore_lease(&lease);
    fs::write(&behavior, "repair").unwrap();
    let approved_start = live
        .orch
        .start_verified_change(&auth(), &approve_request)
        .await
        .unwrap_or_else(|error| panic!("approve start: {error}"));
    let approve_id = approved_start["workId"].as_str().unwrap().to_string();
    let awaiting = settle(&live.orch, &approve_id).await;
    assert_eq!(awaiting["state"], "awaiting_approval");
    let approve_work = live
        .orch
        .store()
        .load_work_item(&approve_id)
        .unwrap()
        .unwrap();
    live.orch
        .approve_work(
            &auth(),
            "approve-cut-decision",
            lane,
            &workspace,
            &approve_id,
            Some("approve the exact candidate".into()),
            Some(approve_work.revision),
        )
        .await
        .unwrap_or_else(|error| panic!("approve: {error}"));
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );
    let approved_attempts = live
        .orch
        .store()
        .list_work_attempts(Some(&approve_id))
        .unwrap()
        .len();
    assert_eq!(approved_attempts, 1);

    live = reopen_production_store(
        live.host, live.orch, &workspace, &fake, &isolate, &identity, &lease,
    )
    .await;
    let approved = live
        .orch
        .store()
        .load_work_item(&approve_id)
        .unwrap()
        .unwrap();
    assert_eq!(approved.state, WorkState::AwaitingApproval);
    assert!(approved
        .approval
        .as_ref()
        .is_some_and(|approval| approval.candidate_digest.is_some()));
    assert_eq!(
        live.orch
            .store()
            .list_work_attempts(Some(&approve_id))
            .unwrap()
            .len(),
        approved_attempts
    );
    let approve_status = live
        .orch
        .verified_change_status(&auth(), lane, &workspace, &approve_id)
        .unwrap();
    assert_eq!(approve_status["phases"]["checksPassed"], true);
    assert_eq!(approve_status["phases"]["humanApproved"], true);
    assert_eq!(approve_status["phases"]["applied"], false);
    assert_eq!(
        approve_status["safeAction"],
        "Approval is recorded. Application is a separate action."
    );
    assert_secret_free(&approve_status);
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );
    let approve_again = live
        .orch
        .start_verified_change(&auth(), &approve_request)
        .await
        .unwrap_or_else(|error| panic!("approve reconnect: {error}"));
    assert_eq!(approve_again["workId"], approve_id);
    assert_eq!(approve_again["attemptCount"], approved_attempts);

    let digest = approved
        .result
        .as_ref()
        .and_then(|result| result.candidate_verification.as_ref())
        .unwrap()
        .content_digest
        .clone();
    let applied = live
        .orch
        .apply_verified_change(
            &auth(),
            "apply-cut",
            lane,
            &workspace,
            &approve_id,
            &digest,
            Some(approved.revision),
        )
        .await
        .unwrap_or_else(|error| panic!("apply: {error}"));
    assert_eq!(applied["work"]["state"], "succeeded");
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_AFTER
    );
    let applied_attempts = live
        .orch
        .store()
        .list_work_attempts(Some(&approve_id))
        .unwrap()
        .len();

    live = reopen_production_store(
        live.host, live.orch, &workspace, &fake, &isolate, &identity, &lease,
    )
    .await;
    let succeeded = live
        .orch
        .store()
        .load_work_item(&approve_id)
        .unwrap()
        .unwrap();
    assert_eq!(succeeded.state, WorkState::Succeeded);
    assert_eq!(
        live.orch
            .store()
            .list_work_attempts(Some(&approve_id))
            .unwrap()
            .len(),
        applied_attempts
    );
    let apply_status = live
        .orch
        .verified_change_status(&auth(), lane, &workspace, &approve_id)
        .unwrap();
    assert_eq!(apply_status["phases"]["applied"], true);
    assert_eq!(apply_status["phases"]["humanApproved"], true);
    assert_eq!(
        apply_status["safeAction"],
        "The exact candidate was applied. No further application is safe."
    );
    assert_secret_free(&apply_status);
    assert_eq!(
        (
            fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
            fs::read_to_string(workspace.join("src/report.rs")).unwrap(),
        ),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );
    let apply_again = live
        .orch
        .start_verified_change(&auth(), &approve_request)
        .await
        .unwrap_or_else(|error| panic!("apply reconnect: {error}"));
    assert_eq!(apply_again["workId"], approve_id);
    assert_eq!(apply_again["attemptCount"], applied_attempts);
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_AFTER
    );

    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_after_candidate_persistence_and_verification() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let behavior = harness.fake_dir.path().join("behavior");
    let lane = harness.lane;
    let persist_request = harness.request("persist-cut", "isolated_review", "macos");
    let verify_request = harness.request("verify-cut", "isolated_review", "macos");

    harness.set_behavior("ledger-only");
    let persisted = harness
        .orch
        .start_verified_change(&auth(), &persist_request)
        .await
        .unwrap_or_else(|error| panic!("persist start: {error}"));
    let persist_id = persisted["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &persist_id).await;
    assert_eq!(settled["state"], "review");
    let persisted_work = harness
        .orch
        .store()
        .load_work_item(&persist_id)
        .unwrap()
        .unwrap();
    let persisted_verification = persisted_work
        .result
        .as_ref()
        .and_then(|result| result.candidate_verification.as_ref())
        .expect("candidate verification");
    assert!(persisted_verification.change_proposed);
    assert!(!persisted_verification.checks_passed);
    assert!(!persisted_verification.applied);
    let persisted_digest = persisted_verification.content_digest.clone();
    assert_eq!(
        harness
            .orch
            .store()
            .list_work_attempts(Some(&persist_id))
            .unwrap()
            .len(),
        1
    );
    let snapshot = harness
        .orch
        .store()
        .candidate_snapshot_dir(&persist_id)
        .unwrap();
    assert_eq!(
        fs::read_to_string(snapshot.join("src/ledger.rs")).unwrap(),
        LEDGER_AFTER
    );
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );

    let mut live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let persisted_again = live
        .orch
        .store()
        .load_work_item(&persist_id)
        .unwrap()
        .unwrap();
    assert_eq!(persisted_again.state, WorkState::Review);
    assert_eq!(
        persisted_again
            .result
            .as_ref()
            .and_then(|result| result.candidate_verification.as_ref())
            .map(|verification| verification.content_digest.clone()),
        Some(persisted_digest.clone())
    );
    assert_eq!(
        live.orch
            .store()
            .list_work_attempts(Some(&persist_id))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fs::read_to_string(
            live.orch
                .store()
                .candidate_snapshot_dir(&persist_id)
                .unwrap()
                .join("src/ledger.rs")
        )
        .unwrap(),
        LEDGER_AFTER
    );
    let persist_status = live
        .orch
        .verified_change_status(&auth(), lane, &workspace, &persist_id)
        .unwrap();
    assert_eq!(persist_status["phases"]["workerStopped"], true);
    assert_eq!(persist_status["phases"]["changeProposed"], true);
    assert_eq!(persist_status["phases"]["checksPassed"], false);
    assert_eq!(persist_status["phases"]["humanApproved"], false);
    assert_eq!(persist_status["phases"]["applied"], false);
    assert_eq!(persist_status["attemptCount"], 1);
    assert_eq!(
        persist_status["safeAction"],
        "The worker stopped without verified checks. Do not treat the model verdict as success."
    );
    assert_secret_free(&persist_status);
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );
    let persist_again = live
        .orch
        .start_verified_change(&auth(), &persist_request)
        .await
        .unwrap_or_else(|error| panic!("persist reconnect: {error}"));
    assert_eq!(persist_again["workId"], persist_id);
    assert_eq!(persist_again["attemptCount"], 1);

    restore_lease(&lease);
    fs::write(&behavior, "repair").unwrap();
    let verified = live
        .orch
        .start_verified_change(&auth(), &verify_request)
        .await
        .unwrap_or_else(|error| panic!("verify start: {error}"));
    let verify_id = verified["workId"].as_str().unwrap().to_string();
    let awaiting = settle(&live.orch, &verify_id).await;
    assert_eq!(awaiting["state"], "awaiting_approval");
    let verified_work = live
        .orch
        .store()
        .load_work_item(&verify_id)
        .unwrap()
        .unwrap();
    let verified_verification = verified_work
        .result
        .as_ref()
        .and_then(|result| result.candidate_verification.as_ref())
        .expect("verified candidate");
    assert!(verified_verification.checks_passed);
    assert!(verified_work.approval.is_none());
    let verified_digest = verified_verification.content_digest.clone();
    assert_eq!(
        live.orch
            .store()
            .list_work_attempts(Some(&verify_id))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );

    live = reopen_production_store(
        live.host, live.orch, &workspace, &fake, &isolate, &identity, &lease,
    )
    .await;
    let verified_again = live
        .orch
        .store()
        .load_work_item(&verify_id)
        .unwrap()
        .unwrap();
    assert_eq!(verified_again.state, WorkState::AwaitingApproval);
    assert!(verified_again.approval.is_none());
    assert_eq!(
        verified_again
            .result
            .as_ref()
            .and_then(|result| result.candidate_verification.as_ref())
            .map(|verification| verification.content_digest.clone()),
        Some(verified_digest)
    );
    assert_eq!(
        live.orch
            .store()
            .list_work_attempts(Some(&verify_id))
            .unwrap()
            .len(),
        1
    );
    let verify_status = live
        .orch
        .verified_change_status(&auth(), lane, &workspace, &verify_id)
        .unwrap();
    assert_eq!(verify_status["phases"]["workerStopped"], true);
    assert_eq!(verify_status["phases"]["changeProposed"], true);
    assert_eq!(verify_status["phases"]["checksPassed"], true);
    assert_eq!(verify_status["phases"]["humanApproved"], false);
    assert_eq!(verify_status["phases"]["applied"], false);
    assert_eq!(verify_status["attemptCount"], 1);
    assert_eq!(
        verify_status["safeAction"],
        "Review the candidate diff and required checks. Approval does not apply the change."
    );
    assert_secret_free(&verify_status);
    assert_eq!(
        (
            fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
            fs::read_to_string(workspace.join("src/report.rs")).unwrap(),
        ),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    let verify_again = live
        .orch
        .start_verified_change(&auth(), &verify_request)
        .await
        .unwrap_or_else(|error| panic!("verify reconnect: {error}"));
    assert_eq!(verify_again["workId"], verify_id);
    assert_eq!(verify_again["attemptCount"], 1);
    assert_eq!(verify_again["phases"]["checksPassed"], true);
    assert_eq!(verify_again["phases"]["humanApproved"], false);

    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

async fn mcp_tool(
    addr: std::net::SocketAddr,
    token: &str,
    id: u64,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .send()
        .await
        .expect("mcp post")
        .json()
        .await
        .expect("mcp json")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_status_apply_and_discard_bind_the_exact_digest() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("repair");
    let server = start_control_server(harness.orch.clone(), 0).await.unwrap();
    let session_id = harness.lane.to_string();
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("operator-review", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_eq!(settled["state"], "awaiting_approval");
    let workspace = settled["workspace"].as_str().unwrap().to_string();

    let status = mcp_tool(
        server.addr,
        "verified-change-token",
        1,
        "ptah_verified_change_status",
        serde_json::json!({
            "request_id": "status-1",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": work_id,
        }),
    )
    .await;
    let view = &status["result"]["structuredContent"];
    assert_eq!(view["phases"]["checksPassed"], true, "{status}");
    assert_eq!(view["phases"]["humanApproved"], false);
    assert_eq!(view["phases"]["applied"], false);
    assert_eq!(view["workState"], "awaiting_approval");
    assert!(view["boundedDiff"].as_str().unwrap().contains("ledger"));
    let digest = view["candidateDigest"].as_str().unwrap().to_string();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );

    let premature = mcp_tool(
        server.addr,
        "verified-change-token",
        2,
        "ptah_apply_verified_change",
        serde_json::json!({
            "request_id": "apply-early",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": work_id,
            "candidate_digest": digest,
        }),
    )
    .await;
    assert!(premature.get("error").is_some(), "{premature}");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    let wrong = mcp_tool(
        server.addr,
        "verified-change-token",
        3,
        "ptah_discard_verified_change",
        serde_json::json!({
            "request_id": "discard-wrong",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": work_id,
            "candidate_digest": "sha256:not-the-candidate",
        }),
    )
    .await;
    assert!(wrong.get("error").is_some(), "{wrong}");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );

    let discarded = mcp_tool(
        server.addr,
        "verified-change-token",
        4,
        "ptah_discard_verified_change",
        serde_json::json!({
            "request_id": "discard-exact",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": work_id,
            "candidate_digest": digest,
        }),
    )
    .await;
    let discarded_view = &discarded["result"]["structuredContent"];
    assert!(discarded.get("error").is_none(), "{discarded}");
    assert_eq!(discarded_view["phases"]["applied"], false);
    assert_eq!(discarded_view["workState"], "cancelled");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );

    restore_lease(&harness.fake_dir.path().join("lease.json"));
    let second = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("operator-apply", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let apply_id = second["workId"].as_str().unwrap().to_string();
    let awaiting = settle(&harness.orch, &apply_id).await;
    assert_eq!(awaiting["state"], "awaiting_approval");
    let apply_status = mcp_tool(
        server.addr,
        "verified-change-token",
        5,
        "ptah_verified_change_status",
        serde_json::json!({
            "request_id": "status-2",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": apply_id,
        }),
    )
    .await;
    let apply_view = &apply_status["result"]["structuredContent"];
    let apply_digest = apply_view["candidateDigest"].as_str().unwrap().to_string();
    let revision = apply_view["workRevision"].as_u64();
    harness
        .orch
        .approve_work(
            &auth(),
            "operator-approve",
            harness.lane,
            harness.workspace.path(),
            &apply_id,
            Some("approve the exact candidate".into()),
            revision,
        )
        .await
        .unwrap();
    let applied = mcp_tool(
        server.addr,
        "verified-change-token",
        6,
        "ptah_apply_verified_change",
        serde_json::json!({
            "request_id": "apply-exact",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": apply_id,
            "candidate_digest": apply_digest,
        }),
    )
    .await;
    let applied_view = &applied["result"]["structuredContent"];
    assert!(applied.get("error").is_none(), "{applied}");
    assert_eq!(applied_view["phases"]["applied"], true);
    assert_eq!(applied_view["candidateDigest"], apply_digest);
    assert_eq!(
        harness.source_pair(),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );
    let stale = mcp_tool(
        server.addr,
        "verified-change-token",
        7,
        "ptah_apply_verified_change",
        serde_json::json!({
            "request_id": "apply-wrong",
            "session_id": session_id,
            "workspace": workspace,
            "work_id": apply_id,
            "candidate_digest": "sha256:not-the-candidate",
        }),
    )
    .await;
    assert!(stale.get("error").is_some(), "{stale}");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );

    let _ = server.stop_and_wait().await;
    harness.close().await;
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
if [ "$behavior" = "ledger-only" ]; then
  mkdir -p src
  printf '%s' 'pub fn balance(cents: &[i32]) -> i32 {
    cents.iter().sum()
}
' > src/ledger.rs
fi
if [ "$behavior" = "repair" ] || [ "$behavior" = "hold" ] || [ "$behavior" = "escape" ] || [ "$behavior" = "survive" ] || [ "$behavior" = "add-files" ] || [ "$behavior" = "extra" ] || [ "$behavior" = "add-kept" ] || [ "$behavior" = "add-fresh" ] || [ "$behavior" = "add-pair" ]; then
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
if [ "$behavior" = "add-files" ]; then
  printf '%s' 'pub fn note() -> i32 { 1 }
' > src/aaa_note.rs
fi
if [ "$behavior" = "add-kept" ]; then
  mkdir -p kept
  printf '%s' 'pub fn note() -> i32 { 1 }
' > kept/note.rs
fi
if [ "$behavior" = "add-fresh" ]; then
  mkdir -p fresh
  printf '%s' 'pub fn note() -> i32 { 1 }
' > fresh/note.rs
fi
if [ "$behavior" = "add-pair" ]; then
  mkdir -p kept fresh/nested
  printf '%s' 'pub fn note() -> i32 { 1 }
' > kept/note.rs
  printf '%s' 'pub fn note() -> i32 { 1 }
' > fresh/a.rs
  printf '%s' 'pub fn note() -> i32 { 1 }
' > fresh/nested/b.rs
fi
if [ "$behavior" = "extra" ]; then
  printf '%s' 'pub fn extra() {}
' > src/notes.rs
fi
if [ "$behavior" = "survive" ]; then
  ( sleep 2; printf 'SURVIVOR\n' >> src/ledger.rs ) >/dev/null 2>&1 &
  sleep 61 >/dev/null 2>&1 &
fi
printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"repaired the balance pair\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"text":"repaired the balance pair\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
"#;

struct ResetFault;
impl Drop for ResetFault {
    fn drop(&mut self) {
        APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        CLEANUP_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        *grokptah_agent_bridge::DURABLE_WRITE_FAULT
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }
}

async fn approved_repair(label: &str) -> (Harness, String, String, u64) {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(&auth(), &harness.request(label, "isolated_review", "macos"))
        .await
        .unwrap_or_else(|error| panic!("start {label}: {error}"));
    let work_id = started["workId"].as_str().unwrap().to_string();
    let _ = settle(&harness.orch, &work_id).await;
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let digest = work
        .result
        .as_ref()
        .and_then(|result| result.candidate_verification.as_ref())
        .unwrap_or_else(|| {
            panic!(
                "no verification for {label}: state {:?} failure {:?}",
                work.state,
                work.result
                    .as_ref()
                    .and_then(|result| result.failure.clone())
            )
        })
        .content_digest
        .clone();
    harness
        .orch
        .approve_work(
            &auth(),
            "approve-repair",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            Some("approve".into()),
            Some(work.revision),
        )
        .await
        .unwrap();
    let revision = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .revision;
    (harness, work_id, digest, revision)
}

async fn approved_files(
    label: &str,
    behavior: &str,
    files: Vec<String>,
) -> (Harness, String, String, u64) {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior(behavior);
    let started = harness
        .orch
        .start_verified_change(&auth(), &harness.request_files(label, files))
        .await
        .unwrap_or_else(|error| panic!("start {label}: {error}"));
    let work_id = started["workId"].as_str().unwrap().to_string();
    let _ = settle(&harness.orch, &work_id).await;
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let digest = work
        .result
        .as_ref()
        .and_then(|result| result.candidate_verification.as_ref())
        .unwrap_or_else(|| {
            panic!(
                "no verification for {label}: state {:?} failure {:?}",
                work.state,
                work.result
                    .as_ref()
                    .and_then(|result| result.failure.clone())
            )
        })
        .content_digest
        .clone();
    harness
        .orch
        .approve_work(
            &auth(),
            "approve-files",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            Some("approve".into()),
            Some(work.revision),
        )
        .await
        .unwrap();
    let revision = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .revision;
    (harness, work_id, digest, revision)
}

fn adding_files() -> Vec<String> {
    vec![
        "src/aaa_note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_path_failure_rolls_back_and_a_crash_is_not_a_clean_noop() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("rollback").await;
    APPLY_FAULT.store(9, std::sync::atomic::Ordering::SeqCst);
    let rolled = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-rollback",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(rolled.to_string().contains("rolled back"), "{rolled}");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.into(), REPORT_BEFORE.into())
    );
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let crashed = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-crash",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap_err();
    assert!(crashed.to_string().contains("reconciliation"), "{crashed}");
    let partial = harness.source_pair();
    assert_ne!(partial, (LEDGER_AFTER.into(), REPORT_AFTER.into()));
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let again = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let replay = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-replay",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(again.revision),
        )
        .await
        .unwrap_err();
    assert!(replay.to_string().contains("reconciliation"), "{replay}");
    assert_eq!(harness.source_pair(), partial);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_status_does_not_reveal_or_mutate_the_candidate() {
    let (harness, work_id, _digest, _revision) = approved_repair("scope").await;
    let before = fs::read_dir(harness.orch.store().root().join("work-items"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    let other = harness.host.session_new_kind(SessionKind::Build).unwrap();
    harness
        .host
        .session_set_cwd(other.id, harness.workspace.path())
        .unwrap();
    let foreign =
        harness
            .orch
            .verified_change_status(&auth(), other.id, harness.workspace.path(), &work_id);
    let unknown = harness.orch.verified_change_status(
        &auth(),
        harness.lane,
        harness.workspace.path(),
        "missing-work",
    );
    let foreign_error = foreign.unwrap_err().to_string();
    let unknown_error = unknown.unwrap_err().to_string();
    assert_eq!(foreign_error, unknown_error);
    assert!(!foreign_error.contains("COORDINATED_REPORT"));
    assert!(!foreign_error.contains("iter().sum()"));
    let other_approve = harness
        .orch
        .approve_work(
            &auth(),
            "approve-foreign",
            other.id,
            harness.workspace.path(),
            &work_id,
            Some("clear".into()),
            Some(0),
        )
        .await
        .unwrap_err()
        .to_string();
    let missing_approve = harness
        .orch
        .approve_work(
            &auth(),
            "approve-missing",
            harness.lane,
            harness.workspace.path(),
            "missing-work",
            Some("clear".into()),
            Some(0),
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(other_approve, missing_approve);
    assert!(!other_approve.contains("COORDINATED_REPORT"));
    let malformed = harness
        .orch
        .verified_change_status(&auth(), harness.lane, harness.workspace.path(), "../secret")
        .unwrap_err()
        .to_string();
    assert!(!malformed.contains("COORDINATED_REPORT"));
    assert!(!malformed.contains("iter().sum()"));
    let after = fs::read_dir(harness.orch.store().root().join("work-items"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(before, after);
    let still = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert!(still.approval.as_ref().is_some_and(|approval| {
        approval.candidate_digest.is_some() && approval.reviewer_id == "operator"
    }));
    harness.close().await;
}

static BARRIER_REPO: Mutex<Option<PathBuf>> = Mutex::new(None);
static HOOK_STORE: Mutex<Option<PathBuf>> = Mutex::new(None);
static FOREIGN_AUTHORITY: Mutex<Option<(PathBuf, Vec<u8>)>> = Mutex::new(None);

fn append_tree_mismatch() {
    let Some(root) = HOOK_STORE.lock().unwrap().clone() else {
        return;
    };
    let Ok(entries) = fs::read_dir(root.join("verified-changes")) else {
        return;
    };
    for entry in entries.flatten() {
        let ledger = entry.path().join("tree").join("src").join("ledger.rs");
        if !ledger.is_file() {
            continue;
        }
        let mut bytes = fs::read(&ledger).unwrap();
        bytes.extend(b"\n// tree-mismatch\n");
        fs::write(&ledger, bytes).unwrap();
    }
}

fn reseal_network_and_output() {
    let Some(root) = HOOK_STORE.lock().unwrap().clone() else {
        return;
    };
    let Ok(entries) = fs::read_dir(root.join("verified-changes")) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(mut authority) = read_check_authority(&entry.path()) else {
            continue;
        };
        authority.network = "qualified".into();
        authority.output_limit_bytes = 1;
        let _ = write_check_authority(&entry.path(), &authority.seal());
    }
}

fn copy_foreign_authority() {
    let Some((source, bytes)) = FOREIGN_AUTHORITY.lock().unwrap().clone() else {
        return;
    };
    let Some(root) = HOOK_STORE.lock().unwrap().clone() else {
        return;
    };
    let Ok(entries) = fs::read_dir(root.join("verified-changes")) else {
        return;
    };
    let source = dunce::canonicalize(&source).ok();
    for entry in entries.flatten() {
        let path = entry.path().join("check-authority.json");
        if !path.is_file() {
            continue;
        }
        if dunce::canonicalize(&path).ok() == source {
            continue;
        }
        let _ = fs::write(path, &bytes);
    }
}

struct ClearBindHook;
impl Drop for ClearBindHook {
    fn drop(&mut self) {
        *BEFORE_CANDIDATE_BIND.lock().unwrap() = None;
        *HOOK_STORE.lock().unwrap() = None;
        *FOREIGN_AUTHORITY.lock().unwrap() = None;
    }
}

struct EnableDrive;
impl Drop for EnableDrive {
    fn drop(&mut self) {
        SKIP_VERIFIED_DRIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

fn commit_barrier_b() {
    let Some(repo) = BARRIER_REPO.lock().unwrap().clone() else {
        return;
    };
    fs::write(repo.join("src/ledger.rs"), "COMMIT_B_MARKER\n").unwrap();
    let _ = Command::new("/usr/bin/git")
        .args(["add", "src/ledger.rs"])
        .current_dir(&repo)
        .status();
    let _ = Command::new("/usr/bin/git")
        .args(["commit", "-m", "barrier B"])
        .current_dir(&repo)
        .env("GIT_AUTHOR_NAME", "barrier")
        .env("GIT_AUTHOR_EMAIL", "barrier@grokptah.invalid")
        .env("GIT_COMMITTER_NAME", "barrier")
        .env("GIT_COMMITTER_EMAIL", "barrier@grokptah.invalid")
        .status();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verification_stays_bound_to_the_launch_sha() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let launch = harness.identity.head_sha.clone();
    *BARRIER_REPO.lock().unwrap() = Some(harness.workspace.path().to_path_buf());
    *BEFORE_CANDIDATE_BIND.lock().unwrap() = Some(commit_barrier_b);
    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("barrier", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let _ = settle(&harness.orch, &work_id).await;
    *BEFORE_CANDIDATE_BIND.lock().unwrap() = None;
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let verification = work
        .result
        .as_ref()
        .unwrap()
        .candidate_verification
        .as_ref()
        .unwrap();
    assert_eq!(verification.source_revision, launch);
    let head = git(harness.workspace.path(), &["rev-parse", "HEAD"]);
    assert_ne!(head, launch);
    let approved = harness
        .orch
        .approve_work(
            &auth(),
            "approve-b",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            None,
            Some(work.revision),
        )
        .await;
    assert!(approved.is_err(), "{approved:?}");
    assert_eq!(
        fs::read_to_string(harness.workspace.path().join("src/ledger.rs")).unwrap(),
        "COMMIT_B_MARKER\n"
    );
    harness.close().await;
}

fn directory_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                files.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    bytes,
                ));
            }
        }
    }
    files.sort();
    files
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_request_with_a_changed_profile_conflicts() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("repair");
    let request = harness.request("same-profile", "isolated_review", "macos");
    harness
        .orch
        .start_verified_change(&auth(), &request)
        .await
        .unwrap();
    let agents = directory_bytes(&harness.orch.store().root().join("agents"));
    let specs = directory_bytes(&harness.orch.store().root().join("agent-specs"));
    install_profile(
        harness.orch.store().root(),
        &harness.check.executable,
        harness.oracle.path(),
        2,
    );
    let conflict = harness
        .orch
        .start_verified_change(&auth(), &request)
        .await
        .unwrap_err();
    assert!(
        conflict.to_string().contains("different payload"),
        "{conflict}"
    );
    assert_eq!(
        agents,
        directory_bytes(&harness.orch.store().root().join("agents"))
    );
    assert_eq!(
        specs,
        directory_bytes(&harness.orch.store().root().join("agent-specs"))
    );
    harness.close().await;
}

fn intent_files(root: &Path) -> Vec<String> {
    let dir = root.join("apply-source-intents");
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    names
}

async fn apply_at(
    harness: &Harness,
    request_id: &str,
    work_id: &str,
    digest: &str,
    revision: u64,
) -> Result<serde_json::Value, grokptah_agent_bridge::orchestration::OrchError> {
    harness
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            harness.lane,
            harness.workspace.path(),
            work_id,
            digest,
            Some(revision),
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_faults_before_effect_retry_once_and_a_failed_command_changes_nothing() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("fault-before").await;
    let before = harness.source_pair();
    APPLY_FAULT.store(1, std::sync::atomic::Ordering::SeqCst);
    let early = apply_at(&harness, "apply-fault-1", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(early.to_string().contains("before the intent"), "{early}");
    assert!(intent_files(harness.orch.store().root()).is_empty());
    assert_eq!(harness.source_pair(), before);
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let armed = apply_at(&harness, "apply-fault-2", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(
        armed.to_string().contains("before any source effect"),
        "{armed}"
    );
    assert_eq!(intent_files(harness.orch.store().root()).len(), 1);
    assert_eq!(harness.source_pair(), before);
    APPLY_FAULT.store(6, std::sync::atomic::Ordering::SeqCst);
    let failed = apply_at(&harness, "apply-fault-6", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("not changed"), "{failed}");
    assert!(!failed.to_string().contains("reconciliation"), "{failed}");
    assert_eq!(harness.source_pair(), before);
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert!(work.state != WorkState::Succeeded);
    assert!(
        !work
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let applied = apply_at(&harness, "apply-fault-retry", &work_id, &digest, revision)
        .await
        .unwrap();
    assert_eq!(applied["work"]["state"], "succeeded");
    assert_eq!(
        harness.source_pair(),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_failure_blocks_automatic_continuation() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("fault-rollback").await;
    APPLY_FAULT.store(7, std::sync::atomic::Ordering::SeqCst);
    let failed = apply_at(&harness, "apply-fault-7", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(
        failed.to_string().contains("rollback also failed"),
        "{failed}"
    );
    let poisoned = harness.source_pair();
    assert_ne!(poisoned, (LEDGER_BEFORE.into(), REPORT_BEFORE.into()));
    assert_ne!(poisoned, (LEDGER_AFTER.into(), REPORT_AFTER.into()));
    assert!(poisoned.0.contains("rollback-poison") || poisoned.1.contains("rollback-poison"));
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert!(
        current
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    let replay = apply_at(
        &harness,
        "apply-fault-7-replay",
        &work_id,
        &digest,
        current.revision,
    )
    .await
    .unwrap_err();
    assert!(replay.to_string().contains("reconciliation"), "{replay}");
    assert_eq!(harness.source_pair(), poisoned);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_after_source_effect_commits_without_applying_again() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("fault-restart").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, "apply-fault-4", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(
        interrupted.to_string().contains("before the work commit"),
        "{interrupted}"
    );
    let applied_bytes = (
        fs::read(workspace.join("src/ledger.rs")).unwrap(),
        fs::read(workspace.join("src/report.rs")).unwrap(),
    );
    assert_eq!(applied_bytes.0, LEDGER_AFTER.as_bytes());
    assert_eq!(applied_bytes.1, REPORT_AFTER.as_bytes());
    assert_ne!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .applied
    );
    assert_eq!(
        fs::read(workspace.join("src/ledger.rs")).unwrap(),
        applied_bytes.0
    );
    assert_eq!(
        fs::read(workspace.join("src/report.rs")).unwrap(),
        applied_bytes.1
    );
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            "apply-fault-4-replay",
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(recovered.revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(
        fs::read(workspace.join("src/ledger.rs")).unwrap(),
        applied_bytes.0
    );
    assert_eq!(
        fs::read(workspace.join("src/report.rs")).unwrap(),
        applied_bytes.1
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_after_work_commit_finishes_the_idempotency_response() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("fault-response").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    APPLY_FAULT.store(5, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, "apply-fault-5", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(
        interrupted
            .to_string()
            .contains("before the idempotency response"),
        "{interrupted}"
    );
    let applied_bytes = (
        fs::read(workspace.join("src/ledger.rs")).unwrap(),
        fs::read(workspace.join("src/report.rs")).unwrap(),
    );
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            "apply-fault-5",
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(
        fs::read(workspace.join("src/ledger.rs")).unwrap(),
        applied_bytes.0
    );
    assert_eq!(
        fs::read(workspace.join("src/report.rs")).unwrap(),
        applied_bytes.1
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

struct ClearImmutable(PathBuf);
impl Drop for ClearImmutable {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/chflags")
            .args(["-R", "nouchg"])
            .arg(&self.0)
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_cleanup_failure_is_not_a_completed_discard() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let agents = directory_bytes(&harness.orch.store().root().join("agents"));
    let specs = directory_bytes(&harness.orch.store().root().join("agent-specs"));
    let works_before = directory_bytes(&harness.orch.store().root().join("work-items"));
    let mut foreign = harness.request("unauth-prepare", "isolated_review", "macos");
    foreign.workspace = std::env::temp_dir();
    foreign.session_id = Uuid::nil();
    let refused = harness
        .orch
        .prepare_verified_change(&auth(), &foreign)
        .unwrap_err();
    assert!(refused.to_string().contains("unknown session"), "{refused}");
    assert_eq!(
        agents,
        directory_bytes(&harness.orch.store().root().join("agents"))
    );
    assert_eq!(
        specs,
        directory_bytes(&harness.orch.store().root().join("agent-specs"))
    );
    assert_eq!(
        works_before,
        directory_bytes(&harness.orch.store().root().join("work-items"))
    );
    harness.close().await;

    let (harness, work_id, digest, revision) = approved_repair("discard-cleanup").await;
    let private = harness
        .orch
        .store()
        .verified_change_private_dir(&work_id)
        .unwrap();
    let _clear = ClearImmutable(private.clone());
    let flagged = Command::new("/usr/bin/chflags")
        .args(["-R", "uchg"])
        .arg(&private)
        .status()
        .unwrap();
    assert!(flagged.success(), "chflags could not pin the candidate");
    let failed = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-pinned",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("cleanup failed"), "{failed}");
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert_ne!(work.state, WorkState::Cancelled);
    assert!(work.approval.is_some());
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    drop(_clear);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dirty_source_names_the_worktree_and_does_not_dispatch() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let ledger = harness.workspace.path().join("src/ledger.rs");
    let mut bytes = fs::read(&ledger).unwrap();
    bytes.push(b'x');
    fs::write(&ledger, bytes).unwrap();
    let prepared = harness
        .orch
        .prepare_verified_change(
            &auth(),
            &harness.request("dirty-byte", "isolated_review", "macos"),
        )
        .unwrap();
    let reasons = prepared["readiness"]["reasons"].to_string();
    assert!(reasons.contains("dirty"), "{prepared}");
    assert!(!reasons.contains("2000"), "{prepared}");
    assert!(!reasons.contains("32 MiB"), "{prepared}");
    assert_eq!(prepared["readiness"]["workersDispatched"], 0);
    assert_eq!(prepared["readiness"]["providerInvocations"], 0);
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("dirty-byte", "isolated_review", "macos"),
        )
        .await
        .unwrap_err();
    assert!(started.to_string().contains("not ready"), "{started}");
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_bound_source_names_the_path_ceiling_before_dispatch() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    for index in 0..=2000 {
        fs::write(
            harness.workspace.path().join(format!("extra-{index}.txt")),
            b"x",
        )
        .unwrap();
    }
    let prepared = harness
        .orch
        .prepare_verified_change(
            &auth(),
            &harness.request("too-many-paths", "isolated_review", "macos"),
        )
        .unwrap();
    let reasons = prepared["readiness"]["reasons"].to_string();
    assert!(reasons.contains("2000"), "{prepared}");
    assert_eq!(prepared["readiness"]["workersDispatched"], 0);
    assert_eq!(prepared["readiness"]["providerInvocations"], 0);
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("too-many-paths", "isolated_review", "macos"),
        )
        .await
        .unwrap_err();
    assert!(started.to_string().contains("not ready"), "{started}");
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    harness.close().await;
}

fn candidate_dir(harness: &Harness, work_id: &str) -> PathBuf {
    harness
        .orch
        .store()
        .verified_change_private_dir(work_id)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_check_sandbox_is_not_ready() {
    *CHECK_CONFINEMENT_EXECUTABLE.lock().unwrap() = Some(PathBuf::from("/no/such/sandbox-exec"));
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let prepared = harness
        .orch
        .prepare_verified_change(
            &auth(),
            &harness.request("no-sandbox", "isolated_review", "macos"),
        )
        .unwrap();
    let reasons = prepared["readiness"]["reasons"].to_string();
    assert!(reasons.contains("sandbox-exec"), "{prepared}");
    assert_eq!(prepared["readiness"]["workersDispatched"], 0);
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("no-sandbox", "isolated_review", "macos"),
        )
        .await
        .unwrap_err();
    assert!(started.to_string().contains("not ready"), "{started}");
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    *CHECK_CONFINEMENT_EXECUTABLE.lock().unwrap() = None;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_patch_cannot_apply() {
    let (harness, work_id, digest, revision) = approved_repair("tamper-patch").await;
    let before = harness.source_pair();
    fs::write(
        candidate_dir(&harness, &work_id).join("promotion.patch"),
        b"not a patch\n",
    )
    .unwrap();
    let error = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-tamper-patch",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bundle"), "{error}");
    assert_eq!(harness.source_pair(), before);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resealed_patch_after_approval_cannot_apply() {
    let (harness, work_id, digest, revision) = approved_repair("reseal-patch").await;
    let before = harness.source_pair();
    let private_dir = candidate_dir(&harness, &work_id);
    fs::write(
        private_dir.join("promotion.patch"),
        b"tampered patch bytes\n",
    )
    .unwrap();
    let snapshot = private_dir.join("tree");
    let derived = derived_snapshot_fingerprint(&snapshot).unwrap();
    let manifest_path = private_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["finalFingerprint"] = serde_json::json!(derived);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let verification = work
        .result
        .as_ref()
        .unwrap()
        .candidate_verification
        .as_ref()
        .unwrap();
    let bundle =
        recompute_candidate_apply_bundle(&snapshot, verification, &work.policy.allowed_files)
            .unwrap();
    let item_path = fs::read_dir(harness.orch.store().root().join("work-items"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap_or_default())
                .ok()
                .is_some_and(|value| {
                    value.get("workId").and_then(|id| id.as_str()) == Some(work_id.as_str())
                })
        })
        .expect("work item file");
    let mut item: serde_json::Value =
        serde_json::from_slice(&fs::read(&item_path).unwrap()).unwrap();
    item["result"]["candidateVerification"]["applyBundleDigest"] = serde_json::json!(bundle);
    fs::write(&item_path, serde_json::to_vec(&item).unwrap()).unwrap();
    let error = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-resealed-patch",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("bundle") || error.to_string().contains("approval"),
        "{error}"
    );
    assert_eq!(harness.source_pair(), before);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_final_fingerprint_cannot_apply() {
    let (harness, work_id, digest, revision) = approved_repair("tamper-fp").await;
    let before = harness.source_pair();
    let manifest = candidate_dir(&harness, &work_id).join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["finalFingerprint"] = serde_json::json!("deadbeef");
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-tamper-fp",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bundle"), "{error}");
    assert_eq!(harness.source_pair(), before);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_manifest_path_mode_or_blob_cannot_apply() {
    let (harness, work_id, digest, revision) = approved_repair("tamper-manifest").await;
    let before = harness.source_pair();
    let manifest = candidate_dir(&harness, &work_id).join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["manifest"][0]["afterMode"] = serde_json::json!("100755");
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-tamper-mode",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bundle"), "{error}");
    assert_eq!(harness.source_pair(), before);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_materialized_tree_does_not_hide_a_replaced_patch() {
    let (harness, work_id, digest, revision) = approved_repair("hide-patch").await;
    let before = harness.source_pair();
    let dir = candidate_dir(&harness, &work_id);
    let tree = fs::read(dir.join("tree/src/ledger.rs")).unwrap();
    fs::write(dir.join("promotion.patch"), b"replaced patch\n").unwrap();
    assert_eq!(fs::read(dir.join("tree/src/ledger.rs")).unwrap(), tree);
    let error = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-hide-patch",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bundle"), "{error}");
    assert_eq!(harness.source_pair(), before);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_bundle_tamper_clears_approval_and_changes_no_source() {
    let (harness, work_id, _digest, _revision) = approved_repair("clear-approval").await;
    let before = harness.source_pair();
    fs::write(
        candidate_dir(&harness, &work_id).join("promotion.patch"),
        b"tampered\n",
    )
    .unwrap();
    harness
        .orch
        .verified_change_status(&auth(), harness.lane, harness.workspace.path(), &work_id)
        .unwrap();
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert!(
        work.approval.is_none(),
        "approval remained after the bundle changed"
    );
    assert_eq!(harness.source_pair(), before);
    harness.close().await;
}

fn rewrite_only_intent(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let dir = root.join("apply-source-intents");
    let path = fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_apply_intent_final_fingerprint_cannot_fabricate_success() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("intent-fp").await;
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-intent-fp",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["finalFingerprint"] = serde_json::json!("fabricated");
    });
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_ne!(recovered.state, WorkState::Succeeded);
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_after_source_effect_before_work_commit_finishes_apply() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("discard-applied").await;
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-discard-4",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-applied",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already applied"), "{error}");
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert_eq!(work.state, WorkState::Succeeded);
    assert_eq!(
        harness.source_pair(),
        (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string())
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_after_intent_before_effect_retires_intent_then_cancels() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("discard-intent").await;
    let before = harness.source_pair();
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-discard-2",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-intent",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert_eq!(work.state, WorkState::Cancelled);
    assert_eq!(harness.source_pair(), before);
    let intents = harness.orch.store().root().join("apply-source-intents");
    assert!(
        fs::read_dir(&intents)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
            == 0
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_refuses_poisoned_apply() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("discard-poison").await;
    APPLY_FAULT.store(7, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-discard-7",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-poison",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconciliation"), "{error}");
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert_ne!(work.state, WorkState::Cancelled);
    assert!(
        harness.source_pair().0.contains("rollback-poison")
            || harness.source_pair().1.contains("rollback-poison")
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_execution_envelope_is_ineligible_without_side_effects() {
    SKIP_VERIFIED_DRIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("no-envelope", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let specs = directory_bytes(&harness.orch.store().root().join("agent-specs"));
    fs::remove_file(candidate_dir(&harness, &work_id).join("execution-envelope.json")).unwrap();
    SKIP_VERIFIED_DRIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    harness.orch.drive_native_executor_once().await;
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    assert_eq!(
        specs,
        directory_bytes(&harness.orch.store().root().join("agent-specs"))
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_execution_budget_or_profile_cannot_dispatch() {
    SKIP_VERIFIED_DRIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("tamper-budget", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let path = candidate_dir(&harness, &work_id).join("execution-envelope.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["budget"] = serde_json::json!("high_assurance");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    SKIP_VERIFIED_DRIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    harness.orch.drive_native_executor_once().await;
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_execution_envelope_cannot_enable_grok_for_unrelated_work() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let specs = directory_bytes(&harness.orch.store().root().join("agent-specs"));
    let created = harness
        .orch
        .create_work(
            &auth(),
            "unrelated-work",
            harness.lane,
            harness.workspace.path(),
            "task".into(),
            "unrelated".into(),
            0,
            None,
            None,
            Vec::new(),
            WorkPolicy::default(),
        )
        .await
        .unwrap();
    let work_id = created["work"]["workId"].as_str().unwrap().to_string();
    let revision = created["work"]["revision"].as_u64();
    harness
        .orch
        .assign_work(
            &auth(),
            "assign-unrelated",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            Some(harness.agent_id.clone()),
            revision,
        )
        .await
        .unwrap();
    let dir = candidate_dir(&harness, &work_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("execution-envelope.json"), br#"{"schemaVersion":1,"workId":"foreign","budget":"economy","envelopeDigest":"sha256:nope"}"#).unwrap();
    harness.orch.drive_native_executor_once().await;
    assert!(harness
        .orch
        .store()
        .live_managed_intent_for_work(&work_id)
        .unwrap()
        .is_none());
    assert_eq!(
        specs,
        directory_bytes(&harness.orch.store().root().join("agent-specs"))
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_apply_intent_schema_fails_closed() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("intent-schema").await;
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-schema",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["schemaVersion"] = serde_json::json!(99);
    });
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_ne!(
        live.orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_apply_intent_cannot_finish_a_newer_work_revision() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("intent-stale").await;
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-stale",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["workRevision"] = serde_json::json!(9_999);
    });
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_ne!(
        live.orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_apply_intent_patch_or_candidate_digest_cannot_recover() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("intent-patch").await;
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-patch-digest",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["patchDigest"] = serde_json::json!("0000");
        value["candidateDigest"] = serde_json::json!("sha256:other");
    });
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_ne!(
        live.orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    assert_eq!(
        (
            fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
            fs::read_to_string(workspace.join("src/report.rs")).unwrap()
        ),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_apply_intent_cannot_complete_another_receipt() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("intent-foreign").await;
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = harness
        .orch
        .apply_verified_change(
            &auth(),
            "apply-foreign",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["requestId"] = serde_json::json!("someone-else");
        value["sessionId"] = serde_json::json!("00000000-0000-0000-0000-000000000000");
    });
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_ne!(
        live.orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_apply_and_discard_converge_to_one_truthful_result() {
    let (harness, work_id, digest, revision) = approved_repair("converge").await;
    let orch = harness.orch.clone();
    let lane = harness.lane;
    let workspace = harness.workspace.path().to_path_buf();
    let apply_id = work_id.clone();
    let discard_id = work_id.clone();
    let apply_digest = digest.clone();
    let discard_digest = digest.clone();
    let apply = {
        let orch = orch.clone();
        let workspace = workspace.clone();
        tokio::spawn(async move {
            orch.apply_verified_change(
                &auth(),
                "apply-converge",
                lane,
                &workspace,
                &apply_id,
                &apply_digest,
                Some(revision),
            )
            .await
        })
    };
    let discard = {
        let orch = orch.clone();
        let workspace = workspace.clone();
        tokio::spawn(async move {
            orch.discard_verified_change(
                &auth(),
                "discard-converge",
                lane,
                &workspace,
                &discard_id,
                &discard_digest,
                Some(revision),
            )
            .await
        })
    };
    let _ = apply.await.unwrap();
    let _ = discard.await.unwrap();
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let source = harness.source_pair();
    let before = (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string());
    let after = (LEDGER_AFTER.to_string(), REPORT_AFTER.to_string());
    match work.state {
        WorkState::Succeeded => assert_eq!(source, after),
        WorkState::Cancelled => assert_eq!(source, before),
        other => panic!("converged to {other:?} with source {source:?}"),
    }
    harness.close().await;
}

fn diff_paths(patch: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(patch).lines() {
        let Some(rest) = line.strip_prefix("diff --git ") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let Some(a) = parts.next() else { continue };
        let Some(b) = parts.next() else { continue };
        let path = b
            .strip_prefix("b/")
            .or_else(|| a.strip_prefix("a/"))
            .unwrap_or(b);
        paths.push(path.to_string());
    }
    paths.sort();
    paths.dedup();
    paths
}

fn manifest_paths(private_dir: &Path) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(private_dir.join("manifest.json")).unwrap()).unwrap();
    let mut paths = value["manifest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_exit_with_surviving_mutator_cannot_retain_a_candidate() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("survive");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("survive", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    let _ = Command::new("/usr/bin/pkill")
        .args(["-f", "sleep 61"])
        .status();
    assert_ne!(settled["state"], "awaiting_approval", "{settled}");
    assert!(!candidate_dir(&harness, &work_id)
        .join("promotion.patch")
        .exists());
    assert!(!candidate_dir(&harness, &work_id).join("tree").exists());
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    let listed = Command::new("/usr/bin/pgrep")
        .args(["-f", "sleep 61"])
        .output()
        .unwrap();
    assert!(
        listed.stdout.is_empty(),
        "the surviving mutator was left running"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preverification_patch_tree_mismatch_cannot_verify() {
    let _clear = ClearBindHook;
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    *HOOK_STORE.lock().unwrap() = Some(harness.orch.store().root().to_path_buf());
    *BEFORE_CANDIDATE_BIND.lock().unwrap() = Some(append_tree_mismatch);
    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("mismatch", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_ne!(settled["state"], "awaiting_approval", "{settled}");
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let verification = work
        .result
        .as_ref()
        .unwrap()
        .candidate_verification
        .as_ref()
        .unwrap();
    assert!(!verification.checks_passed);
    assert!(verification
        .checks
        .iter()
        .all(|check| check.outcome != "passed" && check.exit_code.is_none()));
    let tree = fs::read_to_string(
        candidate_dir(&harness, &work_id)
            .join("tree")
            .join("src")
            .join("ledger.rs"),
    )
    .unwrap();
    assert!(tree.contains("tree-mismatch"));
    assert!(tree.contains("iter().sum()"));
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applied_patch_must_reproduce_the_checked_materialized_tree() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("reproduce", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_eq!(settled["state"], "awaiting_approval", "{settled}");
    let private_dir = candidate_dir(&harness, &work_id);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(private_dir.join("manifest.json")).unwrap()).unwrap();
    let patch = fs::read(private_dir.join("promotion.patch")).unwrap();
    let base = manifest["baseRevision"].as_str().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let checkout = scratch.path().join("checkout");
    let added = Command::new("/usr/bin/git")
        .args(["worktree", "add", "--detach", "--force"])
        .arg(&checkout)
        .arg(base)
        .current_dir(harness.workspace.path())
        .status()
        .unwrap();
    assert!(added.success());
    let mut child = Command::new("/usr/bin/git")
        .args(["apply", "--binary", "--whitespace=nowarn"])
        .current_dir(&checkout)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(&patch).unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    for entry in manifest["manifest"].as_array().unwrap() {
        let path = entry["path"].as_str().unwrap();
        let checked = private_dir.join("tree").join(path);
        let reproduced = checkout.join(path);
        if entry["state"] == "delete" {
            assert!(!checked.exists());
            assert!(!reproduced.exists());
            continue;
        }
        assert_eq!(fs::read(&checked).unwrap(), fs::read(&reproduced).unwrap());
        use std::os::unix::fs::PermissionsExt;
        let checked_mode = fs::symlink_metadata(&checked).unwrap().permissions().mode() & 0o777;
        let reproduced_mode = fs::symlink_metadata(&reproduced)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(checked_mode, reproduced_mode, "{path}");
    }
    let _ = Command::new("/usr/bin/git")
        .args(["worktree", "remove", "--force"])
        .arg(&checkout)
        .current_dir(harness.workspace.path())
        .status();
    let stored = harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .into_iter()
        .find(|intent| intent.work_id == work_id)
        .and_then(|intent| intent.grok)
        .unwrap();
    let tree = private_dir.join("tree");
    assert_eq!(
        stored.diff_digest.as_deref(),
        Some(retained_candidate_diff_digest(&tree).unwrap().as_str())
    );
    assert_eq!(stored.changed_paths, manifest_paths(&private_dir));
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_paths_must_equal_manifest_and_allowed_scope() {
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("paths", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_eq!(settled["state"], "awaiting_approval", "{settled}");
    let private_dir = candidate_dir(&harness, &work_id);
    let patch = diff_paths(&fs::read(private_dir.join("promotion.patch")).unwrap());
    let manifest = manifest_paths(&private_dir);
    let mut allowed = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .policy
        .allowed_files;
    allowed.sort();
    assert_eq!(patch, manifest);
    assert_eq!(patch, allowed);
    harness.close().await;

    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("extra");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("extra", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_ne!(settled["state"], "awaiting_approval", "{settled}");
    assert!(!candidate_dir(&harness, &work_id)
        .join("promotion.patch")
        .exists());
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_first_untracked_add_is_not_classified_not_applied() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) =
        approved_files("first-add", "add-files", adding_files()).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let crashed = apply_at(&harness, "apply-first-add", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(crashed.to_string().contains("reconciliation"), "{crashed}");
    assert!(
        !crashed.to_string().contains("was not changed"),
        "{crashed}"
    );
    let note = harness.workspace.path().join("src/aaa_note.rs");
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    assert_eq!(harness.source_pair().0, LEDGER_BEFORE);
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert!(
        work.result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    assert_ne!(work.state, WorkState::Succeeded);
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_ne!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/aaa_note.rs")).unwrap(),
        NOTE_AFTER
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_BEFORE
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_all_new_files_before_index_update_recovers_applied() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) =
        approved_files("all-new", "add-files", adding_files()).await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, "apply-all-new", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(
        interrupted.to_string().contains("before the work commit"),
        "{interrupted}"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/aaa_note.rs")).unwrap(),
        NOTE_AFTER
    );
    let removed = Command::new("/usr/bin/git")
        .args(["update-index", "--force-remove", "--", "src/aaa_note.rs"])
        .current_dir(&workspace)
        .status()
        .unwrap();
    assert!(removed.success());
    let indexed = Command::new("/usr/bin/git")
        .args(["ls-files", "--error-unmatch", "--", "src/aaa_note.rs"])
        .current_dir(&workspace)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!indexed.success(), "the new file was still in the index");
    assert_ne!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .applied
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/aaa_note.rs")).unwrap(),
        NOTE_AFTER
    );
    let still_unindexed = Command::new("/usr/bin/git")
        .args(["ls-files", "--error-unmatch", "--", "src/aaa_note.rs"])
        .current_dir(&workspace)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!still_unindexed.success());
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_never_leaves_an_untracked_candidate_file() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) =
        approved_files("discard-add", "add-files", adding_files()).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let crashed = apply_at(&harness, "apply-discard-add", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(crashed.to_string().contains("reconciliation"), "{crashed}");
    let note = harness.workspace.path().join("src/aaa_note.rs");
    assert!(note.is_file());
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-untracked",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap();
    assert!(!note.exists(), "discard left the untracked candidate file");
    assert_eq!(harness.source_pair().0, LEDGER_BEFORE);
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert_eq!(work.state, WorkState::Cancelled);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_untracked_file_during_apply_requires_reconciliation() {
    let (harness, work_id, digest, revision) =
        approved_files("foreign-file", "add-files", adding_files()).await;
    fs::write(harness.workspace.path().join("FOREIGN.txt"), b"foreign\n").unwrap();
    let error = apply_at(&harness, "apply-foreign", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconciliation"), "{error}");
    assert!(!harness.workspace.path().join("src/aaa_note.rs").exists());
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    assert!(harness.workspace.path().join("FOREIGN.txt").is_file());
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    assert!(
        work.result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    assert_ne!(work.state, WorkState::Succeeded);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resealed_check_authority_cannot_upgrade_network_or_resource_policy() {
    let _clear = ClearBindHook;
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    *HOOK_STORE.lock().unwrap() = Some(harness.orch.store().root().to_path_buf());
    *BEFORE_CANDIDATE_BIND.lock().unwrap() = Some(reseal_network_and_output);
    harness.set_behavior("repair");
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("reseal-net", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let settled = settle(&harness.orch, &work_id).await;
    assert_ne!(settled["state"], "awaiting_approval", "{settled}");
    let private_dir = candidate_dir(&harness, &work_id);
    let envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(private_dir.join("execution-envelope.json")).unwrap())
            .unwrap();
    let original = envelope["checkAuthorityDigest"]
        .as_str()
        .unwrap()
        .to_string();
    let resealed = read_check_authority(&private_dir).unwrap();
    assert_eq!(resealed.network, "qualified");
    assert_eq!(resealed.output_limit_bytes, 1);
    assert_ne!(resealed.authority_digest, original);
    let work = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let verification = work
        .result
        .as_ref()
        .unwrap()
        .candidate_verification
        .as_ref()
        .unwrap();
    assert_eq!(verification.check_authority_digest, original);
    assert_ne!(
        verification.check_authority_digest,
        resealed.authority_digest
    );
    assert!(!verification.checks_passed);
    assert!(verification
        .checks
        .iter()
        .all(|check| check.outcome != "passed" && check.exit_code.is_none()));
    assert!(!verification.apply_bundle_digest.is_empty());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_check_authority_cannot_be_copied_to_another_work() {
    let _clear = ClearBindHook;
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    harness.set_behavior("repair");
    let first = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("foreign-a", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let first_id = first["workId"].as_str().unwrap().to_string();
    let first_settled = settle(&harness.orch, &first_id).await;
    assert_eq!(
        first_settled["state"], "awaiting_approval",
        "{first_settled}"
    );
    let first_dir = candidate_dir(&harness, &first_id);
    let first_authority = fs::read(first_dir.join("check-authority.json")).unwrap();
    let first_digest = read_check_authority(&first_dir).unwrap().authority_digest;
    *FOREIGN_AUTHORITY.lock().unwrap() = Some((
        first_dir.join("check-authority.json"),
        first_authority.clone(),
    ));
    *HOOK_STORE.lock().unwrap() = Some(harness.orch.store().root().to_path_buf());
    *BEFORE_CANDIDATE_BIND.lock().unwrap() = Some(copy_foreign_authority);
    let second = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("foreign-b", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let second_id = second["workId"].as_str().unwrap().to_string();
    let second_settled = settle(&harness.orch, &second_id).await;
    assert_ne!(
        second_settled["state"], "awaiting_approval",
        "{second_settled}"
    );
    let second_dir = candidate_dir(&harness, &second_id);
    let envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(second_dir.join("execution-envelope.json")).unwrap())
            .unwrap();
    let original = envelope["checkAuthorityDigest"].as_str().unwrap();
    assert_ne!(original, first_digest);
    let copied = read_check_authority(&second_dir).unwrap();
    assert_eq!(copied.authority_digest, first_digest);
    assert_ne!(copied.work_id, second_id);
    let work = harness
        .orch
        .store()
        .load_work_item(&second_id)
        .unwrap()
        .unwrap();
    let verification = work
        .result
        .as_ref()
        .unwrap()
        .candidate_verification
        .as_ref()
        .unwrap();
    assert_eq!(verification.check_authority_digest, original);
    assert_ne!(verification.check_authority_digest, first_digest);
    assert!(!verification.checks_passed);
    assert!(verification
        .checks
        .iter()
        .all(|check| check.exit_code.is_none() && check.outcome != "passed"));
    assert_eq!(
        fs::read(first_dir.join("check-authority.json")).unwrap(),
        first_authority
    );
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&first_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::AwaitingApproval
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authority_identity_mismatch_runs_no_check() {
    let _drive = EnableDrive;
    SKIP_VERIFIED_DRIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let harness = Harness::open(ManagedExecutionBudgetProfile::Economy);
    let started = harness
        .orch
        .start_verified_change(
            &auth(),
            &harness.request("identity", "isolated_review", "macos"),
        )
        .await
        .unwrap();
    let work_id = started["workId"].as_str().unwrap().to_string();
    let private_dir = candidate_dir(&harness, &work_id);
    let envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(private_dir.join("execution-envelope.json")).unwrap())
            .unwrap();
    let original = envelope["checkAuthorityDigest"]
        .as_str()
        .unwrap()
        .to_string();
    let mut authority = read_check_authority(&private_dir).unwrap();
    authority.work_id = "other-work".into();
    write_check_authority(&private_dir, &authority.seal()).unwrap();
    assert_ne!(
        read_check_authority(&private_dir).unwrap().authority_digest,
        original
    );
    SKIP_VERIFIED_DRIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    harness.orch.drive_native_executor_once().await;
    assert!(harness
        .orch
        .store()
        .list_managed_intents()
        .unwrap()
        .is_empty());
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .attempt_count,
        0
    );
    let argv = fs::read_to_string(harness.fake_dir.path().join("grok.argv")).unwrap_or_default();
    assert!(
        !argv.contains("--session-id"),
        "identity mismatch dispatched a worker: {argv}"
    );
    assert!(!private_dir.join("tree").exists());
    assert_eq!(
        harness.source_pair(),
        (LEDGER_BEFORE.to_string(), REPORT_BEFORE.to_string())
    );
    harness.close().await;
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_file_at_candidate_add_path_is_preserved_and_discard_refuses() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) =
        approved_files("foreign-add-path", "add-files", adding_files()).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-foreign-path", &work_id, &digest, revision)
        .await
        .unwrap_err();
    let note = harness.workspace.path().join("src/aaa_note.rs");
    fs::write(&note, b"foreign-bytes\n").unwrap();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-foreign-path",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconciliation"), "{error}");
    assert_eq!(fs::read(&note).unwrap(), b"foreign-bytes\n");
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_symlink_at_candidate_add_path_is_preserved() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) =
        approved_files("foreign-link", "add-files", adding_files()).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-foreign-link", &work_id, &digest, revision)
        .await
        .unwrap_err();
    let note = harness.workspace.path().join("src/aaa_note.rs");
    fs::remove_file(&note).unwrap();
    std::os::unix::fs::symlink("ledger.rs", &note).unwrap();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-foreign-link",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconciliation"), "{error}");
    assert!(note.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(&note).unwrap(), Path::new("ledger.rs"));
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_partial_candidate_addition_can_be_removed() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) =
        approved_files("exact-partial", "add-files", adding_files()).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-exact-partial", &work_id, &digest, revision)
        .await
        .unwrap_err();
    let note = harness.workspace.path().join("src/aaa_note.rs");
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-exact-partial",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap();
    assert!(!note.exists());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_add_cleanup_does_not_remove_a_preexisting_empty_directory() {
    let _reset = ResetFault;
    let files = vec![
        "kept/note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    let (harness, work_id, digest, revision) =
        approved_files("preexisting-dir", "add-kept", files).await;
    fs::create_dir(harness.workspace.path().join("kept")).unwrap();
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-kept", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(harness.workspace.path().join("kept/note.rs").is_file());
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-kept",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap();
    assert!(!harness.workspace.path().join("kept/note.rs").exists());
    assert!(harness.workspace.path().join("kept").is_dir());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_add_cleanup_does_not_leave_a_candidate_created_directory() {
    let _reset = ResetFault;
    let files = vec![
        "fresh/note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    let (harness, work_id, digest, revision) =
        approved_files("created-dir", "add-fresh", files).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-fresh", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(harness.workspace.path().join("fresh/note.rs").is_file());
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-fresh",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current.revision),
        )
        .await
        .unwrap();
    assert!(!harness.workspace.path().join("fresh").exists());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_head_after_source_effect_cannot_recover_success() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("head-after").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-head-after", &work_id, &digest, revision)
        .await
        .unwrap_err();
    git(
        &workspace,
        &["commit", "--allow-empty", "-m", "foreign head"],
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_ne!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_head_with_candidate_bytes_and_foreign_commit_is_poisoned() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("foreign-commit").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(
        &harness,
        "apply-foreign-commit",
        &work_id,
        &digest,
        revision,
    )
    .await
    .unwrap_err();
    git(&workspace, &["add", "src/ledger.rs", "src/report.rs"]);
    git(&workspace, &["commit", "-m", "foreign tree"]);
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_ne!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        LEDGER_AFTER
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_foreign_change_on_manifest_path_blocks_apply() {
    let (harness, work_id, digest, revision) = approved_repair("staged-apply").await;
    let ledger = harness.workspace.path().join("src/ledger.rs");
    fs::write(&ledger, b"fn foreign() {}\n").unwrap();
    git(harness.workspace.path(), &["add", "src/ledger.rs"]);
    let error = apply_at(&harness, "apply-staged", &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconciliation"), "{error}");
    assert_eq!(fs::read(&ledger).unwrap(), b"fn foreign() {}\n");
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_foreign_change_on_manifest_path_blocks_recovery() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("staged-recover").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(
        &harness,
        "apply-staged-recover",
        &work_id,
        &digest,
        revision,
    )
    .await
    .unwrap_err();
    fs::write(workspace.join("src/ledger.rs"), b"fn staged() {}\n").unwrap();
    git(&workspace, &["add", "src/ledger.rs"]);
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_ne!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_added_file_application_leaves_index_unchanged() {
    let (harness, work_id, digest, revision) =
        approved_files("index-clean", "add-files", adding_files()).await;
    let applied = apply_at(&harness, "apply-index", &work_id, &digest, revision)
        .await
        .unwrap();
    assert_eq!(applied["work"]["state"], "succeeded");
    let workspace = harness.workspace.path();
    assert!(git_ok(workspace, &["diff", "--cached", "--quiet", "HEAD"]));
    assert!(!git_ok(
        workspace,
        &["ls-files", "--error-unmatch", "--", "src/aaa_note.rs"]
    ));
    assert_eq!(
        fs::read_to_string(workspace.join("src/aaa_note.rs")).unwrap(),
        NOTE_AFTER
    );
    harness.close().await;
}

fn bump_work_revision(root: &Path, work_id: &str) -> u64 {
    for entry in fs::read_dir(root.join("work-items")).unwrap().flatten() {
        let path = entry.path();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if value["workId"] != work_id {
            continue;
        }
        let revision = value["revision"].as_u64().unwrap() + 7;
        value["revision"] = serde_json::json!(revision);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        return revision;
    }
    panic!("work item {work_id} was not stored");
}

fn receipt_status(root: &Path, request_id: &str) -> Option<String> {
    for path in idempotency_files(root) {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value["requestId"] == request_id
            && value["ownerId"] == "primary"
            && value["response"]["work"]["state"] == "succeeded"
        {
            return value["status"].as_str().map(str::to_string);
        }
    }
    for path in idempotency_files(root) {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value["requestId"] == request_id && value["ownerId"] == "primary" {
            return value["status"].as_str().map(str::to_string);
        }
    }
    None
}

fn reconciliation_required(harness: &Harness, work_id: &str) -> bool {
    harness
        .orch
        .store()
        .load_work_item(work_id)
        .unwrap()
        .unwrap()
        .result
        .unwrap()
        .candidate_verification
        .unwrap()
        .reconciliation_required
}

async fn partial_add(label: &str, behavior: &str, files: Vec<String>) -> (Harness, String, String) {
    let (harness, work_id, digest, revision) = approved_files(label, behavior, files).await;
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let crashed = apply_at(
        &harness,
        &format!("apply-{label}"),
        &work_id,
        &digest,
        revision,
    )
    .await
    .unwrap_err();
    assert!(crashed.to_string().contains("reconciliation"), "{crashed}");
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    (harness, work_id, digest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_intent_with_tampered_manifest_performs_zero_cleanup() {
    let _reset = ResetFault;
    let (harness, work_id, digest) =
        partial_add("stale-manifest", "add-files", adding_files()).await;
    let note = harness.workspace.path().join("src/aaa_note.rs");
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(candidate_dir(&harness, &work_id).join("manifest.json")).unwrap(),
    )
    .unwrap();
    manifest["manifest"][0]["afterBlob"] =
        serde_json::json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    fs::write(
        candidate_dir(&harness, &work_id).join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let revision = bump_work_revision(harness.orch.store().root(), &work_id);
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-stale-manifest",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(
        !error.to_string().contains("cleanup interrupted"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_intent_cannot_expand_cleanup_beyond_allowed_files() {
    let _reset = ResetFault;
    let (harness, work_id, digest) = partial_add("stale-expand", "add-files", adding_files()).await;
    let note = harness.workspace.path().join("src/aaa_note.rs");
    let evil = harness.workspace.path().join("src/evil.rs");
    fs::write(&evil, b"do not delete\n").unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(candidate_dir(&harness, &work_id).join("manifest.json")).unwrap(),
    )
    .unwrap();
    let mut extra = manifest["manifest"][0].clone();
    extra["path"] = serde_json::json!("src/evil.rs");
    manifest["manifest"].as_array_mut().unwrap().push(extra);
    fs::write(
        candidate_dir(&harness, &work_id).join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let revision = bump_work_revision(harness.orch.store().root(), &work_id);
    let _ = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-stale-expand",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    assert_eq!(fs::read_to_string(&evil).unwrap(), "do not delete\n");
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_preexisting_directory_provenance_is_quarantined() {
    let _reset = ResetFault;
    let files = vec![
        "kept/note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    let (harness, work_id, digest, revision) =
        approved_files("provenance", "add-kept", files).await;
    fs::create_dir(harness.workspace.path().join("kept")).unwrap();
    APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, "apply-provenance", &work_id, &digest, revision)
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let note = harness.workspace.path().join("kept/note.rs");
    assert!(note.is_file());
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["preexistingDirectories"] = serde_json::json!([]);
    });
    let current = bump_work_revision(harness.orch.store().root(), &work_id);
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-provenance",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("directory provenance"),
        "{error}"
    );
    assert!(note.is_file());
    assert!(harness.workspace.path().join("kept").is_dir());
    assert!(reconciliation_required(&harness, &work_id));
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_cleanup_requires_the_approved_apply_bundle() {
    let _reset = ResetFault;
    let (harness, work_id, digest) = partial_add("stale-bundle", "add-files", adding_files()).await;
    let note = harness.workspace.path().join("src/aaa_note.rs");
    rewrite_only_intent(harness.orch.store().root(), |value| {
        value["applyBundleDigest"] = serde_json::json!("sha256:not-the-approved-bundle");
    });
    let revision = bump_work_revision(harness.orch.store().root(), &work_id);
    let _ = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-stale-bundle",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_stale_partial_addition_can_still_be_safely_discarded() {
    let _reset = ResetFault;
    let (harness, work_id, digest) =
        partial_add("stale-ordinary", "add-files", adding_files()).await;
    let note = harness.workspace.path().join("src/aaa_note.rs");
    assert_eq!(fs::read_to_string(&note).unwrap(), NOTE_AFTER);
    let revision = bump_work_revision(harness.orch.store().root(), &work_id);
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-stale-ordinary",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert!(!note.exists());
    assert_eq!(harness.source_pair().0, LEDGER_BEFORE);
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Cancelled
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_file_cleanup_before_directory_cleanup_recovers() {
    let _reset = ResetFault;
    let files = vec![
        "fresh/note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    let (harness, work_id, digest) = partial_add("dir-cut", "add-fresh", files).await;
    let fresh = harness.workspace.path().join("fresh");
    assert!(fresh.join("note.rs").is_file());
    CLEANUP_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .revision;
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-dir-cut",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cut 3"), "{error}");
    assert!(!fresh.join("note.rs").exists());
    assert!(fresh.is_dir());
    CLEANUP_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert!(!workspace.join("fresh").exists());
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_before_paths_still_reconcile_candidate_created_directories() {
    let _reset = ResetFault;
    let files = vec![
        "fresh/note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    let (harness, work_id, digest) = partial_add("all-before-dir", "add-fresh", files).await;
    let fresh = harness.workspace.path().join("fresh");
    fs::remove_file(fresh.join("note.rs")).unwrap();
    assert!(fresh.is_dir());
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .revision;
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-all-before-dir",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current),
        )
        .await
        .unwrap();
    assert!(!fresh.exists());
    assert!(!reconciliation_required(&harness, &work_id));
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preexisting_empty_directory_survives_every_cleanup_cut() {
    let _reset = ResetFault;
    let files = vec![
        "kept/note.rs".into(),
        "fresh/a.rs".into(),
        "fresh/nested/b.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    for cut in 1..=6 {
        let (harness, work_id, digest, revision) =
            approved_files(&format!("cut-{cut}"), "add-pair", files.clone()).await;
        fs::create_dir(harness.workspace.path().join("kept")).unwrap();
        APPLY_FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
        let _ = apply_at(
            &harness,
            &format!("apply-cut-{cut}"),
            &work_id,
            &digest,
            revision,
        )
        .await
        .unwrap_err();
        APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        for path in ["kept/note.rs", "fresh/a.rs", "fresh/nested/b.rs"] {
            let full = harness.workspace.path().join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, NOTE_AFTER).unwrap();
        }
        CLEANUP_FAULT.store(cut, std::sync::atomic::Ordering::SeqCst);
        let current = harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .revision;
        let error = harness
            .orch
            .discard_verified_change(
                &auth(),
                &format!("discard-cut-{cut}"),
                harness.lane,
                harness.workspace.path(),
                &work_id,
                &digest,
                Some(current),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(&format!("cut {cut}")),
            "cut {cut}: {error}"
        );
        assert!(
            harness.workspace.path().join("kept").is_dir(),
            "cut {cut} removed the preexisting directory"
        );
        CLEANUP_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        harness.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_receipt_completes_only_after_full_source_restoration() {
    let _reset = ResetFault;
    let files = vec![
        "fresh/note.rs".into(),
        "src/ledger.rs".into(),
        "src/report.rs".into(),
    ];
    let (harness, work_id, digest) = partial_add("receipt-cut", "add-fresh", files).await;
    CLEANUP_FAULT.store(5, std::sync::atomic::Ordering::SeqCst);
    let current = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .revision;
    let error = harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-receipt-cut",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(current),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cut 5"), "{error}");
    assert!(!harness.workspace.path().join("fresh").exists());
    assert_ne!(
        receipt_status(harness.orch.store().root(), "discard-receipt-cut").as_deref(),
        Some("complete")
    );
    CLEANUP_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let revision = harness
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .revision;
    harness
        .orch
        .discard_verified_change(
            &auth(),
            "discard-receipt-done",
            harness.lane,
            harness.workspace.path(),
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(
        receipt_status(harness.orch.store().root(), "discard-receipt-done").as_deref(),
        Some("complete")
    );
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Cancelled
    );
    harness.close().await;
}

fn source_at(workspace: &Path) -> (String, String) {
    (
        fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
        fs::read_to_string(workspace.join("src/report.rs")).unwrap(),
    )
}

fn idempotency_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, paths);
            } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
                paths.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&root.join("idempotency").join("v2"), &mut paths);
    paths.sort();
    paths
}

fn admission_files(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("apply-admissions")) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    names
}

fn primary_receipt(root: &Path, request_id: &str) -> (PathBuf, serde_json::Value, Vec<u8>) {
    let mut found = Vec::new();
    for path in idempotency_files(root) {
        let bytes = fs::read(&path).unwrap();
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value["ownerId"] == "primary" && value["requestId"] == request_id {
            found.push((path, value, bytes));
        }
    }
    if let Some(index) = found
        .iter()
        .position(|(_, value, _)| value["response"]["work"]["state"] == "succeeded")
    {
        return found.swap_remove(index);
    }
    if found.len() == 1 {
        return found.pop().unwrap();
    }
    panic!(
        "missing unambiguous primary receipt for {request_id} ({} candidates)",
        found.len()
    );
}

fn plant_owner_collision(store: &OrchStore, session: Uuid, request_id: &str) -> PathBuf {
    let scope =
        IdempotencyScope::new("intruder", session, Path::new("/foreign/workspace")).unwrap();
    let before = idempotency_files(store.root());
    store
        .claim_idempotency(
            &scope,
            "ptah_apply_verified_change",
            request_id,
            "foreign-owner-payload",
        )
        .unwrap();
    idempotency_files(store.root())
        .into_iter()
        .find(|path| !before.contains(path))
        .expect("foreign owner receipt")
}

fn plant_workspace_collision(store: &OrchStore, session: Uuid, request_id: &str) -> PathBuf {
    let scope = IdempotencyScope::new("primary", session, Path::new("/foreign/workspace")).unwrap();
    let before = idempotency_files(store.root());
    store
        .claim_idempotency(
            &scope,
            "ptah_apply_verified_change",
            request_id,
            "foreign-workspace-payload",
        )
        .unwrap();
    idempotency_files(store.root())
        .into_iter()
        .find(|path| !before.contains(path))
        .expect("foreign workspace receipt")
}

fn freeze_foreign(path: &Path) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    value["status"] = serde_json::json!("complete");
    value["response"] = serde_json::json!({"marker": "foreign-frozen"});
    value["cleanupPlanDigest"] = serde_json::json!("");
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    fs::write(path, &bytes).unwrap();
    fs::read(path).unwrap()
}

async fn stop_live(live: LiveRuntime) {
    live.orch.stop_background_tasks().await;
    live.host.shutdown().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_owner_receipt_collision_is_ignored_and_unchanged() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("foreign-owner").await;
    let request_id = "foreign-owner-apply";
    let foreign = plant_owner_collision(harness.orch.store(), harness.lane, request_id);
    let before = fs::read(&foreign).unwrap();
    let applied = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap();
    assert_eq!(applied["work"]["state"], "succeeded");
    assert_eq!(fs::read(&foreign).unwrap(), before);
    let (_, receipt, _) = primary_receipt(harness.orch.store().root(), request_id);
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["response"]["work"]["state"], "succeeded");
    assert!(!receipt["cleanupPlanDigest"].as_str().unwrap().is_empty());
    let foreign_body: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(foreign_body["ownerId"], "intruder");
    assert_eq!(foreign_body["cleanupPlanDigest"], "");
    assert!(foreign_body["response"].is_null());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_workspace_receipt_collision_is_ignored_and_unchanged() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("foreign-workspace").await;
    let request_id = "foreign-workspace-apply";
    let foreign = plant_workspace_collision(harness.orch.store(), harness.lane, request_id);
    let before = fs::read(&foreign).unwrap();
    let applied = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap();
    assert_eq!(applied["work"]["state"], "succeeded");
    assert_eq!(fs::read(&foreign).unwrap(), before);
    let (authorized, receipt, _) = primary_receipt(harness.orch.store().root(), request_id);
    assert_ne!(authorized, foreign);
    assert_ne!(authorized.parent(), foreign.parent());
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["response"]["work"]["state"], "succeeded");
    assert!(!receipt["cleanupPlanDigest"].as_str().unwrap().is_empty());
    let foreign_body: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(foreign_body["ownerId"], "primary");
    assert_eq!(foreign_body["requestId"], request_id);
    assert_eq!(foreign_body["sessionId"], harness.lane.to_string());
    assert_eq!(foreign_body["cleanupPlanDigest"], "");
    assert!(foreign_body["response"].is_null());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_recovery_completes_only_the_exact_scoped_receipt() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("scoped-recovery").await;
    let request_id = "scoped-recovery-apply";
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let owner_path = plant_owner_collision(harness.orch.store(), lane, request_id);
    let workspace_path = plant_workspace_collision(harness.orch.store(), lane, request_id);
    let owner_bytes = freeze_foreign(&owner_path);
    let workspace_bytes = freeze_foreign(&workspace_path);
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::SourceEffectCompleteBeforeWorkCommit)
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_eq!(fs::read(&owner_path).unwrap(), owner_bytes);
    assert_eq!(fs::read(&workspace_path).unwrap(), workspace_bytes);
    let (_, receipt, _) = primary_receipt(live.orch.store().root(), request_id);
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["response"]["work"]["state"], "succeeded");
    assert!(!receipt["cleanupPlanDigest"].as_str().unwrap().is_empty());
    let owner_body: serde_json::Value = serde_json::from_slice(&owner_bytes).unwrap();
    let workspace_body: serde_json::Value = serde_json::from_slice(&workspace_bytes).unwrap();
    assert_eq!(owner_body["response"]["marker"], "foreign-frozen");
    assert_eq!(workspace_body["response"]["marker"], "foreign-frozen");
    assert_eq!(owner_body["cleanupPlanDigest"], "");
    assert_eq!(workspace_body["cleanupPlanDigest"], "");
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(fs::read(&owner_path).unwrap(), owner_bytes);
    assert_eq!(fs::read(&workspace_path).unwrap(), workspace_bytes);
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receipt_lookup_does_not_scan_unrelated_owner_shards() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("receipt-scan").await;
    let request_id = "receipt-scan-apply";
    let foreign = plant_owner_collision(harness.orch.store(), harness.lane, request_id);
    let decoy = plant_workspace_collision(harness.orch.store(), harness.lane, request_id);
    let foreign_before = fs::read(&foreign).unwrap();
    let decoy_before = fs::read(&decoy).unwrap();
    RECEIPT_TREE_SCANS.store(0, std::sync::atomic::Ordering::SeqCst);
    let applied = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap();
    assert_eq!(applied["work"]["state"], "succeeded");
    assert_eq!(
        RECEIPT_TREE_SCANS.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(fs::read(&foreign).unwrap(), foreign_before);
    assert_eq!(fs::read(&decoy).unwrap(), decoy_before);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_fault_cuts_reopen_without_a_permanent_in_progress_receipt() {
    let _reset = ResetFault;
    for cut in 1..=5 {
        let (harness, work_id, digest, revision) =
            approved_repair(&format!("admission-{cut}")).await;
        let request_id = format!("admission-cut-{cut}");
        let workspace = harness.workspace.path().to_path_buf();
        let source_before = harness.source_pair();
        let fake = harness.fake_dir.path().join("grok");
        let isolate = harness.isolate.path().to_path_buf();
        let identity = harness.identity.clone();
        let lease = harness.fake_dir.path().join("lease.json");
        let lane = harness.lane;
        let root = harness.orch.store().root().to_path_buf();
        ADMISSION_FAULT.store(cut, std::sync::atomic::Ordering::SeqCst);
        let error = apply_at(&harness, &request_id, &work_id, &digest, revision)
            .await
            .unwrap_err();
        ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            harness.source_pair(),
            source_before,
            "cut {cut} touched source"
        );
        if cut == 1 {
            assert_eq!(error.apply_phase(), Some(ApplyPhase::NoSourceEffect));
            assert!(intent_files(&root).is_empty(), "cut {cut}");
            assert!(admission_files(&root).is_empty(), "cut {cut}");
            assert_eq!(
                receipt_status(&root, &request_id).as_deref(),
                Some("failed"),
                "cut {cut}"
            );
        } else {
            assert_eq!(
                error.apply_phase(),
                Some(ApplyPhase::SourceEffectPossible),
                "cut {cut}"
            );
            assert_eq!(
                receipt_status(&root, &request_id).as_deref(),
                Some("pending"),
                "cut {cut}"
            );
            let (_, receipt, _) = primary_receipt(&root, &request_id);
            match cut {
                2 => {
                    assert!(intent_files(&root).is_empty(), "cut {cut}");
                    assert_eq!(admission_files(&root).len(), 1, "cut {cut}");
                    assert_eq!(receipt["cleanupPlanDigest"], "", "cut {cut}");
                }
                3 => {
                    assert!(intent_files(&root).is_empty(), "cut {cut}");
                    assert_eq!(admission_files(&root).len(), 1, "cut {cut}");
                    assert!(!receipt["cleanupPlanDigest"].as_str().unwrap().is_empty());
                }
                4 => {
                    assert_eq!(intent_files(&root).len(), 1, "cut {cut}");
                    assert_eq!(admission_files(&root).len(), 1, "cut {cut}");
                    assert!(!receipt["cleanupPlanDigest"].as_str().unwrap().is_empty());
                }
                5 => {
                    assert_eq!(intent_files(&root).len(), 1, "cut {cut}");
                    assert!(admission_files(&root).is_empty(), "cut {cut}");
                    assert!(!receipt["cleanupPlanDigest"].as_str().unwrap().is_empty());
                }
                _ => unreachable!(),
            }
            if matches!(cut, 2..=4) {
                let names = admission_files(&root);
                let envelope: serde_json::Value = serde_json::from_slice(
                    &fs::read(root.join("apply-admissions").join(&names[0])).unwrap(),
                )
                .unwrap();
                assert_eq!(envelope["schemaVersion"], 1);
                assert_eq!(envelope["workId"], work_id);
                for key in [
                    "sessionId",
                    "workspace",
                    "ownerId",
                    "requestId",
                    "candidateDigest",
                    "cleanupPlanDigest",
                    "receiptPath",
                    "intentPath",
                    "receiptPriorDigest",
                    "receiptNextDigest",
                    "intentNextDigest",
                    "receiptNextText",
                    "intentNextText",
                ] {
                    assert!(
                        envelope[key]
                            .as_str()
                            .is_some_and(|value| !value.is_empty()),
                        "cut {cut} envelope missing {key}"
                    );
                }
            }
        }
        let live = reopen_production_store(
            harness.host,
            harness.orch,
            &workspace,
            &fake,
            &isolate,
            &identity,
            &lease,
        )
        .await;
        let replay = live
            .orch
            .apply_verified_change(
                &auth(),
                &request_id,
                lane,
                &workspace,
                &work_id,
                &digest,
                Some(revision),
            )
            .await
            .unwrap_err();
        let rendered = replay.to_string();
        assert!(
            !rendered.contains("still in progress"),
            "cut {cut}: {rendered}"
        );
        assert_ne!(
            receipt_status(live.orch.store().root(), &request_id).as_deref(),
            Some("pending"),
            "cut {cut}"
        );
        assert_eq!(
            (
                fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
                fs::read_to_string(workspace.join("src/report.rs")).unwrap()
            ),
            source_before,
            "cut {cut}"
        );
        assert_ne!(
            live.orch
                .store()
                .load_work_item(&work_id)
                .unwrap()
                .unwrap()
                .state,
            WorkState::Succeeded,
            "cut {cut}"
        );
        if cut == 1 {
            assert!(
                rendered.contains("before the intent"),
                "cut {cut}: {rendered}"
            );
            assert!(intent_files(live.orch.store().root()).is_empty());
        } else {
            assert!(
                rendered.contains("definitely not applied"),
                "cut {cut}: {rendered}"
            );
            assert!(
                intent_files(live.orch.store().root()).is_empty(),
                "cut {cut}"
            );
            assert!(
                admission_files(live.orch.store().root()).is_empty(),
                "cut {cut}"
            );
        }
        stop_live(live).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fault4_original_request_replays_recovered_success() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("fault4-original").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "fault4-original-request";
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::SourceEffectCompleteBeforeWorkCommit)
    );
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    assert_ne!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let source = (
        fs::read(workspace.join("src/ledger.rs")).unwrap(),
        fs::read(workspace.join("src/report.rs")).unwrap(),
    );
    assert_eq!(source.0, LEDGER_AFTER.as_bytes());
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let recovered = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(recovered.state, WorkState::Succeeded);
    assert!(
        recovered
            .result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .applied
    );
    assert!(intent_files(live.orch.store().root()).is_empty());
    assert_eq!(
        receipt_status(live.orch.store().root(), request_id).as_deref(),
        Some("complete")
    );
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(fs::read(workspace.join("src/ledger.rs")).unwrap(), source.0);
    assert_eq!(fs::read(workspace.join("src/report.rs")).unwrap(), source.1);
    let again = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(again, replay);
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fault5_original_request_replays_recovered_success() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("fault5-original").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "fault5-original-request";
    APPLY_FAULT.store(5, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::WorkCommittedBeforeReceipt)
    );
    assert!(
        interrupted
            .to_string()
            .contains("before the idempotency response"),
        "{interrupted}"
    );
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    assert_eq!(intent_files(harness.orch.store().root()).len(), 1);
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let source = fs::read(workspace.join("src/ledger.rs")).unwrap();
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert!(intent_files(live.orch.store().root()).is_empty());
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(fs::read(workspace.join("src/ledger.rs")).unwrap(), source);
    assert_eq!(
        receipt_status(live.orch.store().root(), request_id).as_deref(),
        Some("complete")
    );
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_intent_before_source_effect_resolves_original_request() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("crash-before-effect").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "crash-before-effect-request";
    let source_before = harness.source_pair();
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::SourceEffectPossible)
    );
    assert_eq!(intent_files(harness.orch.store().root()).len(), 1);
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    let rendered = replay.to_string();
    assert!(rendered.contains("definitely not applied"), "{rendered}");
    assert!(!rendered.contains("still in progress"), "{rendered}");
    assert_eq!(
        (
            fs::read_to_string(workspace.join("src/ledger.rs")).unwrap(),
            fs::read_to_string(workspace.join("src/report.rs")).unwrap()
        ),
        source_before
    );
    assert_ne!(
        live.orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    assert_eq!(
        receipt_status(live.orch.store().root(), request_id).as_deref(),
        Some("failed")
    );
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_applied_recovery_does_not_leave_a_permanent_pending_receipt() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("not-applied-receipt").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "not-applied-receipt-request";
    ADMISSION_FAULT.store(5, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::SourceEffectPossible)
    );
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_eq!(
        receipt_status(live.orch.store().root(), request_id).as_deref(),
        Some("failed")
    );
    let (_, receipt, _) = primary_receipt(live.orch.store().root(), request_id);
    assert!(receipt["error"]["message"]
        .as_str()
        .unwrap()
        .contains("definitely not applied"));
    assert!(intent_files(live.orch.store().root()).is_empty());
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(
        !replay.to_string().contains("still in progress"),
        "{replay}"
    );
    assert!(
        replay.to_string().contains("definitely not applied"),
        "{replay}"
    );
    assert_eq!(
        receipt_status(live.orch.store().root(), request_id).as_deref(),
        Some("failed")
    );
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poisoned_recovery_never_replays_success_or_no_effect() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("poison-original").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "poison-original-request";
    APPLY_FAULT.store(7, std::sync::atomic::Ordering::SeqCst);
    let failed = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        failed.apply_phase(),
        Some(ApplyPhase::ReconciliationRequired)
    );
    let poisoned = harness.source_pair();
    assert!(
        poisoned.0.contains("rollback-poison") || poisoned.1.contains("rollback-poison"),
        "{poisoned:?}"
    );
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    assert_eq!(intent_files(harness.orch.store().root()).len(), 1);
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert_eq!(intent_files(live.orch.store().root()).len(), 1);
    let work = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_ne!(work.state, WorkState::Succeeded);
    assert!(
        work.result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    assert_eq!(
        receipt_status(live.orch.store().root(), request_id).as_deref(),
        Some("failed")
    );
    let receipt_bytes = primary_receipt(live.orch.store().root(), request_id).2;
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    let rendered = replay.to_string();
    assert!(rendered.contains("reconciliation"), "{rendered}");
    assert!(!rendered.contains("definitely not applied"), "{rendered}");
    assert!(!rendered.contains("still in progress"), "{rendered}");
    assert_eq!(source_at(&workspace), poisoned);
    let again = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(again.to_string(), rendered);
    let reopened = reopen_production_store(
        live.host, live.orch, &workspace, &fake, &isolate, &identity, &lease,
    )
    .await;
    assert_eq!(
        primary_receipt(reopened.orch.store().root(), request_id).2,
        receipt_bytes
    );
    assert_eq!(source_at(&workspace), poisoned);
    assert_eq!(intent_files(reopened.orch.store().root()).len(), 1);
    assert_ne!(
        reopened
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    stop_live(reopened).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_store_reopen_is_idempotent_for_receipt_work_and_source() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("reopen-idempotent").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "reopen-idempotent-request";
    APPLY_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::SourceEffectCompleteBeforeWorkCommit)
    );
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let source = source_at(&workspace);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    let first = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(first["work"]["state"], "succeeded");
    let receipt_bytes = primary_receipt(live.orch.store().root(), request_id).2;
    let work_state = live
        .orch
        .store()
        .load_work_item(&work_id)
        .unwrap()
        .unwrap()
        .state;
    assert!(intent_files(live.orch.store().root()).is_empty());
    let again = reopen_production_store(
        live.host, live.orch, &workspace, &fake, &isolate, &identity, &lease,
    )
    .await;
    assert_eq!(
        primary_receipt(again.orch.store().root(), request_id).2,
        receipt_bytes
    );
    assert_eq!(source_at(&workspace), source);
    assert_eq!(
        again
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        work_state
    );
    assert!(intent_files(again.orch.store().root()).is_empty());
    let replay = again
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(source_at(&workspace), source);
    assert_eq!(
        primary_receipt(again.orch.store().root(), request_id).2,
        receipt_bytes
    );
    stop_live(again).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receipt_completion_precedes_intent_removal() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("receipt-before-intent").await;
    let workspace = harness.workspace.path().to_path_buf();
    let fake = harness.fake_dir.path().join("grok");
    let isolate = harness.isolate.path().to_path_buf();
    let identity = harness.identity.clone();
    let lease = harness.fake_dir.path().join("lease.json");
    let lane = harness.lane;
    let request_id = "receipt-before-intent-request";
    APPLY_FAULT.store(8, std::sync::atomic::Ordering::SeqCst);
    let interrupted = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        interrupted.apply_phase(),
        Some(ApplyPhase::WorkCommittedBeforeReceipt)
    );
    assert!(
        interrupted
            .to_string()
            .contains("before the intent was removed"),
        "{interrupted}"
    );
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    assert_eq!(intent_files(harness.orch.store().root()).len(), 1);
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("complete")
    );
    let receipt_bytes = primary_receipt(harness.orch.store().root(), request_id).2;
    let source = harness.source_pair();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let live = reopen_production_store(
        harness.host,
        harness.orch,
        &workspace,
        &fake,
        &isolate,
        &identity,
        &lease,
    )
    .await;
    assert!(intent_files(live.orch.store().root()).is_empty());
    assert_eq!(
        primary_receipt(live.orch.store().root(), request_id).2,
        receipt_bytes
    );
    assert_eq!(source_at(&workspace), source);
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    let settled = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            lane,
            &workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(settled, replay);
    assert_eq!(source_at(&workspace), source);
    assert!(intent_files(live.orch.store().root()).is_empty());
    stop_live(live).await;
}

fn arm_write_fault(needle: &'static str, skips_remaining: u32) {
    *grokptah_agent_bridge::DURABLE_WRITE_FAULT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) =
        Some(grokptah_agent_bridge::DurableWriteFault {
            needle,
            skips_remaining,
        });
}

fn only_json(dir: &Path) -> PathBuf {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
                files.push(entry.path());
            }
        }
    }
    assert_eq!(
        files.len(),
        1,
        "expected one json file in {}",
        dir.display()
    );
    files.pop().unwrap()
}

struct KeptRuntime {
    workspace: PathBuf,
    fake: PathBuf,
    isolate: PathBuf,
    identity: grokptah_agent_sdk::GrokBuildGitIdentity,
    lease: PathBuf,
    lane: Uuid,
}

fn keep_runtime(harness: &Harness) -> KeptRuntime {
    KeptRuntime {
        workspace: harness.workspace.path().to_path_buf(),
        fake: harness.fake_dir.path().join("grok"),
        isolate: harness.isolate.path().to_path_buf(),
        identity: harness.identity.clone(),
        lease: harness.fake_dir.path().join("lease.json"),
        lane: harness.lane,
    }
}

struct ParkedDirs {
    _home: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    _isolate: tempfile::TempDir,
    _oracle: tempfile::TempDir,
    _fake_dir: tempfile::TempDir,
    _env: ProcessEnvGuard,
}

async fn reopen_kept(harness: Harness, kept: &KeptRuntime) -> (LiveRuntime, ParkedDirs) {
    let Harness {
        host,
        orch,
        home,
        workspace,
        isolate,
        oracle,
        fake_dir,
        _env,
        ..
    } = harness;
    let live = reopen_production_store(
        host,
        orch,
        &kept.workspace,
        &kept.fake,
        &kept.isolate,
        &kept.identity,
        &kept.lease,
    )
    .await;
    (
        live,
        ParkedDirs {
            _home: home,
            _workspace: workspace,
            _isolate: isolate,
            _oracle: oracle,
            _fake_dir: fake_dir,
            _env,
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_path_substitution_changes_no_store_records() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admit-path").await;
    let kept = keep_runtime(&harness);
    let request_id = "admit-path-request";
    ADMISSION_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let envelope = only_json(&root.join("apply-admissions"));
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
    value["receiptPath"] = serde_json::json!("stolen/receipt.json");
    fs::write(&envelope, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let receipt_before = primary_receipt(&root, request_id).2;
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert!(!live
        .orch
        .store()
        .root()
        .join("stolen/receipt.json")
        .exists());
    assert_eq!(
        primary_receipt(live.orch.store().root(), request_id).2,
        receipt_before
    );
    assert!(intent_files(live.orch.store().root()).is_empty());
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_prior_receipt_mismatch_preserves_both_destinations() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admit-prior").await;
    let kept = keep_runtime(&harness);
    let request_id = "admit-prior-request";
    ADMISSION_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let (receipt_path, _, _) = primary_receipt(&root, request_id);
    let mut receipt_bytes = fs::read(&receipt_path).unwrap();
    receipt_bytes.extend_from_slice(b"\n ");
    fs::write(&receipt_path, &receipt_bytes).unwrap();
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
    assert!(intent_files(live.orch.store().root()).is_empty());
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_foreign_or_newer_intent_is_not_overwritten() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admit-foreign-intent").await;
    let kept = keep_runtime(&harness);
    let request_id = "admit-foreign-intent-request";
    ADMISSION_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let intent_dir = root.join("apply-source-intents");
    fs::create_dir_all(&intent_dir).unwrap();
    let envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(only_json(&root.join("apply-admissions"))).unwrap())
            .unwrap();
    let intent_path = root.join(envelope["intentPath"].as_str().unwrap());
    let foreign = br#"{"foreign":true,"newer":1}"#;
    fs::write(&intent_path, foreign).unwrap();
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert_eq!(fs::read(&intent_path).unwrap(), foreign);
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_pair_identity_mismatch_performs_zero_writes() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admit-identity").await;
    let kept = keep_runtime(&harness);
    let request_id = "admit-identity-request";
    ADMISSION_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let envelope_path = only_json(&root.join("apply-admissions"));
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(&envelope_path).unwrap()).unwrap();
    let mut intent: serde_json::Value =
        serde_json::from_str(envelope["intentNextText"].as_str().unwrap()).unwrap();
    intent["workId"] = serde_json::json!("foreign-work");
    let text = serde_json::to_string_pretty(&intent).unwrap();
    let digest_hex = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(text.as_bytes()))
    };
    envelope["intentNextText"] = serde_json::json!(text);
    envelope["intentNextDigest"] = serde_json::json!(digest_hex);
    fs::write(
        &envelope_path,
        serde_json::to_vec_pretty(&envelope).unwrap(),
    )
    .unwrap();
    let receipt_before = primary_receipt(&root, request_id).2;
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert_eq!(
        primary_receipt(live.orch.store().root(), request_id).2,
        receipt_before
    );
    assert!(intent_files(live.orch.store().root()).is_empty());
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_already_installed_pair_is_idempotent() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admit-installed").await;
    let kept = keep_runtime(&harness);
    let request_id = "admit-installed-request";
    ADMISSION_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    let _ = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let (live, _parked) = reopen_kept(harness, &kept).await;
    let once = primary_receipt(live.orch.store().root(), request_id).2;
    let intents_once = intent_files(live.orch.store().root());
    let again = reopen_production_store(
        live.host,
        live.orch,
        &kept.workspace,
        &kept.fake,
        &kept.isolate,
        &kept.identity,
        &kept.lease,
    )
    .await;
    assert_eq!(
        primary_receipt(again.orch.store().root(), request_id).2,
        once
    );
    assert_eq!(intent_files(again.orch.store().root()), intents_once);
    stop_live(again).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_write_failure_after_apply_keeps_original_request_recoverable() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("lifecycle-write").await;
    let kept = keep_runtime(&harness);
    let request_id = "lifecycle-write-request";
    arm_write_fault("work-lifecycle-intents", 0);
    let error = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        error.apply_phase(),
        Some(
            grokptah_agent_bridge::orchestration::ApplyPhase::SourceEffectCompleteBeforeWorkCommit
        )
    );
    let applied = source_at(&kept.workspace);
    assert_eq!(applied.0, LEDGER_AFTER);
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    let (live, _parked) = reopen_kept(harness, &kept).await;
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(source_at(&kept.workspace), applied);
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receipt_write_failure_after_work_commit_recovers_original_success() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("receipt-write").await;
    let kept = keep_runtime(&harness);
    let request_id = "receipt-write-request";
    arm_write_fault("idempotency/v2", 1);
    let error = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert_eq!(
        error.apply_phase(),
        Some(grokptah_agent_bridge::orchestration::ApplyPhase::WorkCommittedBeforeReceipt)
    );
    let applied = source_at(&kept.workspace);
    assert_eq!(
        harness
            .orch
            .store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    assert_eq!(
        receipt_status(harness.orch.store().root(), request_id).as_deref(),
        Some("pending")
    );
    let (live, _parked) = reopen_kept(harness, &kept).await;
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(source_at(&kept.workspace), applied);
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_record_write_failure_resolves_without_false_reexecution() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admission-write").await;
    let kept = keep_runtime(&harness);
    let request_id = "admission-write-request";
    let before = source_at(&kept.workspace);
    arm_write_fault("apply-admissions", 0);
    let error = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    assert!(
        error.apply_phase().is_none()
            || error.apply_phase()
                == Some(grokptah_agent_bridge::orchestration::ApplyPhase::NoSourceEffect)
    );
    assert_eq!(source_at(&kept.workspace), before);
    let (live, _parked) = reopen_kept(harness, &kept).await;
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(!replay.to_string().contains("succeeded"), "{replay}");
    assert_eq!(source_at(&kept.workspace), before);
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_reopen_after_real_write_failure_converges() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("write-reopen").await;
    let kept = keep_runtime(&harness);
    let request_id = "write-reopen-request";
    arm_write_fault("idempotency/v2", 1);
    let _ = apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    let applied = source_at(&kept.workspace);
    let (live, _parked) = reopen_kept(harness, &kept).await;
    let once = primary_receipt(live.orch.store().root(), request_id).2;
    let again = reopen_production_store(
        live.host,
        live.orch,
        &kept.workspace,
        &kept.fake,
        &kept.isolate,
        &kept.identity,
        &kept.lease,
    )
    .await;
    assert_eq!(
        primary_receipt(again.orch.store().root(), request_id).2,
        once
    );
    assert_eq!(source_at(&kept.workspace), applied);
    let replay = again
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    let second = again
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(second, replay);
    assert_eq!(replay["work"]["state"], "succeeded");
    assert_eq!(source_at(&kept.workspace), applied);
    stop_live(again).await;
}

fn reseal_admission_receipt(envelope: &mut serde_json::Value, receipt: &serde_json::Value) {
    use sha2::{Digest, Sha256};
    // Recompute integrity so rejection must come from transition authority.
    let text = serde_json::to_string_pretty(receipt).unwrap();
    envelope["receiptNextDigest"] =
        serde_json::json!(format!("{:x}", Sha256::digest(text.as_bytes())));
    envelope["receiptNextText"] = serde_json::json!(text);
}

async fn rejected_admission_successor(label: &str, field: &str, value: serde_json::Value) {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair(label).await;
    let kept = keep_runtime(&harness);
    let request_id = format!("{label}-request");
    ADMISSION_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    apply_at(&harness, &request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let envelope_path = only_json(&root.join("apply-admissions"));
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(&envelope_path).unwrap()).unwrap();
    let mut receipt: serde_json::Value =
        serde_json::from_str(envelope["receiptNextText"].as_str().unwrap()).unwrap();
    receipt[field] = value;
    reseal_admission_receipt(&mut envelope, &receipt);
    fs::write(
        &envelope_path,
        serde_json::to_vec_pretty(&envelope).unwrap(),
    )
    .unwrap();
    let receipt_before = primary_receipt(&root, &request_id).2;
    let source_before = source_at(&kept.workspace);
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert_eq!(
        primary_receipt(&root, &request_id).2,
        receipt_before,
        "receipt destination changed for {field}"
    );
    assert!(
        intent_files(&root).is_empty(),
        "intent destination changed for {field}"
    );
    assert_eq!(source_at(&kept.workspace), source_before);
    assert!(
        envelope_path.is_file(),
        "invalid journal evidence must remain"
    );
    stop_live(live).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_resealed_complete_successor_performs_zero_writes() {
    rejected_admission_successor("admit-complete", "status", serde_json::json!("complete")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_resealed_failed_successor_performs_zero_writes() {
    rejected_admission_successor("admit-failed", "status", serde_json::json!("failed")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_pending_successor_cannot_inject_response_or_error() {
    rejected_admission_successor(
        "admit-response",
        "response",
        serde_json::json!({"fabricated": true}),
    )
    .await;
    rejected_admission_successor(
        "admit-error",
        "error",
        serde_json::json!({"code": "conflict", "message": "fabricated"}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_successor_changes_only_the_authorized_cleanup_binding() {
    for (field, value) in [
        ("runId", serde_json::json!("fabricated-run")),
        ("createdAt", serde_json::json!("2020-01-01T00:00:00Z")),
        ("ownerId", serde_json::json!("foreign-owner")),
        ("sessionId", serde_json::json!(Uuid::new_v4())),
        ("workspaceDigest", serde_json::json!("0".repeat(64))),
        ("requestId", serde_json::json!("foreign-request")),
        ("tool", serde_json::json!("foreign-tool")),
        ("payloadHash", serde_json::json!("foreign-payload")),
    ] {
        rejected_admission_successor(&format!("admit-{field}"), field, value).await;
    }
}

fn move_pending_receipt_to_legacy(root: &Path, request_id: &str) -> (PathBuf, PathBuf, Vec<u8>) {
    let (modern, receipt, bytes) = primary_receipt(root, request_id);
    assert_eq!(receipt["status"], "pending");
    let legacy = modern
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(modern.file_name().unwrap());
    fs::rename(&modern, &legacy).unwrap();
    (modern, legacy, bytes)
}

async fn legacy_interrupted_apply(
    label: &str,
    cut: u8,
    poison: bool,
    conflict: bool,
    admission: bool,
) {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair(label).await;
    let kept = keep_runtime(&harness);
    let request_id = format!("{label}-request");
    if admission {
        ADMISSION_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    } else {
        APPLY_FAULT.store(cut, std::sync::atomic::Ordering::SeqCst);
    }
    apply_at(&harness, &request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let (modern, legacy, legacy_bytes) = move_pending_receipt_to_legacy(&root, &request_id);
    if poison {
        fs::write(kept.workspace.join("src/ledger.rs"), "foreign source\n").unwrap();
    }
    let modern_before = if conflict {
        let mut receipt: serde_json::Value = serde_json::from_slice(&legacy_bytes).unwrap();
        receipt["payloadHash"] = serde_json::json!("foreign-payload");
        let bytes = serde_json::to_vec_pretty(&receipt).unwrap();
        fs::write(&modern, &bytes).unwrap();
        Some(bytes)
    } else {
        None
    };
    let source = source_at(&kept.workspace);
    let (live, _parked) = reopen_kept(harness, &kept).await;
    let work = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    let verification = work
        .result
        .as_ref()
        .unwrap()
        .candidate_verification
        .as_ref()
        .unwrap();
    assert_eq!(verification.reconciliation_required, poison || conflict);
    if conflict {
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        assert_eq!(fs::read(&modern).unwrap(), modern_before.unwrap());
        assert_eq!(intent_files(&root).len(), 1);
    } else {
        assert!(
            !legacy.exists(),
            "exact legacy claim must be adopted before recovery"
        );
        assert_eq!(
            receipt_status(&root, &request_id).as_deref(),
            Some(if cut == 4 && !poison {
                "complete"
            } else {
                "failed"
            })
        );
    }
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            &request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await;
    if cut == 4 && !poison && !conflict {
        assert_eq!(work.state, WorkState::Succeeded);
        assert_eq!(replay.as_ref().unwrap()["work"]["state"], "succeeded");
    } else {
        let message = replay.as_ref().unwrap_err().to_string();
        assert!(
            message.contains(if conflict {
                "conflict"
            } else if poison {
                "reconciliation"
            } else {
                "definitely not applied"
            }),
            "{message}"
        );
    }
    assert_eq!(source_at(&kept.workspace), source);
    let receipt_once = fs::read(&modern).unwrap();
    let work_once = serde_json::to_value(&work).unwrap();
    let again = reopen_production_store(
        live.host,
        live.orch,
        &kept.workspace,
        &kept.fake,
        &kept.isolate,
        &kept.identity,
        &kept.lease,
    )
    .await;
    assert_eq!(fs::read(&modern).unwrap(), receipt_once);
    assert_eq!(
        serde_json::to_value(
            again
                .orch
                .store()
                .load_work_item(&work_id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        work_once
    );
    let repeated = again
        .orch
        .apply_verified_change(
            &auth(),
            &request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await;
    assert_eq!(format!("{repeated:?}"), format!("{replay:?}"));
    assert_eq!(source_at(&kept.workspace), source);
    stop_live(again).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_pending_applied_intent_recovers_original_request_success() {
    legacy_interrupted_apply("legacy-applied", 4, false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_pending_unapplied_intent_resolves_original_request_no_effect() {
    legacy_interrupted_apply("legacy-unapplied", 2, false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_pending_poisoned_intent_reports_reconciliation() {
    legacy_interrupted_apply("legacy-poisoned", 2, true, false, false).await;
    legacy_interrupted_apply("legacy-conflicting", 2, false, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_receipt_adoption_precedes_work_quarantine() {
    legacy_interrupted_apply("legacy-admission", 2, false, false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_reopen_after_legacy_apply_recovery_is_idempotent() {
    legacy_interrupted_apply("legacy-repeated", 4, false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_old_journal_and_partial_pair_recover_idempotently() {
    for cut in [2, 3, 4] {
        let _reset = ResetFault;
        let (harness, work_id, digest, revision) =
            approved_repair(&format!("admit-old-{cut}")).await;
        let kept = keep_runtime(&harness);
        let request_id = format!("admit-old-{cut}-request");
        ADMISSION_FAULT.store(cut, std::sync::atomic::Ordering::SeqCst);
        apply_at(&harness, &request_id, &work_id, &digest, revision)
            .await
            .unwrap_err();
        ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        let root = harness.orch.store().root().to_path_buf();
        let path = only_json(&root.join("apply-admissions"));
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope.as_object_mut().unwrap().remove("receiptPriorText");
        fs::write(&path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        let source = source_at(&kept.workspace);
        let (live, _parked) = reopen_kept(harness, &kept).await;
        assert!(!path.exists(), "old journal cut {cut} did not converge");
        assert!(intent_files(&root).is_empty());
        let receipt = primary_receipt(&root, &request_id).2;
        let replay = live
            .orch
            .apply_verified_change(
                &auth(),
                &request_id,
                kept.lane,
                &kept.workspace,
                &work_id,
                &digest,
                Some(revision),
            )
            .await
            .unwrap_err();
        assert!(replay.to_string().contains("definitely not applied"));
        assert_eq!(source_at(&kept.workspace), source);
        let again = reopen_production_store(
            live.host,
            live.orch,
            &kept.workspace,
            &kept.fake,
            &kept.isolate,
            &kept.identity,
            &kept.lease,
        )
        .await;
        assert_eq!(primary_receipt(&root, &request_id).2, receipt);
        stop_live(again).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_installed_intent_with_prior_receipt_recovers() {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair("admit-intent-first").await;
    let kept = keep_runtime(&harness);
    let request_id = "admit-intent-first-request";
    ADMISSION_FAULT.store(4, std::sync::atomic::Ordering::SeqCst);
    apply_at(&harness, request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(only_json(&root.join("apply-admissions"))).unwrap())
            .unwrap();
    let receipt_path = primary_receipt(&root, request_id).0;
    fs::write(
        &receipt_path,
        envelope["receiptPriorText"].as_str().unwrap(),
    )
    .unwrap();
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert!(admission_files(&root).is_empty());
    assert!(intent_files(&root).is_empty());
    assert_eq!(receipt_status(&root, request_id).as_deref(), Some("failed"));
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(replay.to_string().contains("definitely not applied"));
    stop_live(live).await;
}

async fn legacy_binding_conflict(label: &str, field: &str, value: serde_json::Value) {
    let _reset = ResetFault;
    let (harness, work_id, digest, revision) = approved_repair(label).await;
    let kept = keep_runtime(&harness);
    let request_id = format!("{label}-request");
    APPLY_FAULT.store(2, std::sync::atomic::Ordering::SeqCst);
    apply_at(&harness, &request_id, &work_id, &digest, revision)
        .await
        .unwrap_err();
    APPLY_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    let root = harness.orch.store().root().to_path_buf();
    let (modern, legacy, bytes) = move_pending_receipt_to_legacy(&root, &request_id);
    let mut receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    receipt[field] = value;
    let evidence = serde_json::to_vec_pretty(&receipt).unwrap();
    fs::write(&legacy, &evidence).unwrap();
    let source = source_at(&kept.workspace);
    let (live, _parked) = reopen_kept(harness, &kept).await;
    assert_eq!(
        fs::read(&legacy).unwrap(),
        evidence,
        "legacy evidence changed for {field}"
    );
    assert!(
        !modern.exists(),
        "invalid legacy evidence was adopted for {field}"
    );
    let work = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert!(
        work.result
            .as_ref()
            .unwrap()
            .candidate_verification
            .as_ref()
            .unwrap()
            .reconciliation_required
    );
    let replay = live
        .orch
        .apply_verified_change(
            &auth(),
            &request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert!(
        replay.to_string().contains("reconciliation"),
        "{field}: {replay}"
    );
    assert!(
        !modern.exists(),
        "original known request became a fresh claim for {field}"
    );
    assert_eq!(fs::read(&legacy).unwrap(), evidence);
    let work_once = serde_json::to_value(work).unwrap();
    let again = reopen_production_store(
        live.host,
        live.orch,
        &kept.workspace,
        &kept.fake,
        &kept.isolate,
        &kept.identity,
        &kept.lease,
    )
    .await;
    assert_eq!(fs::read(&legacy).unwrap(), evidence);
    assert!(!modern.exists());
    assert_eq!(
        serde_json::to_value(
            again
                .orch
                .store()
                .load_work_item(&work_id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        work_once
    );
    let repeated = again
        .orch
        .apply_verified_change(
            &auth(),
            &request_id,
            kept.lane,
            &kept.workspace,
            &work_id,
            &digest,
            Some(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(repeated.to_string(), replay.to_string());
    assert_eq!(source_at(&kept.workspace), source);
    stop_live(again).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_binding_owner_and_request_conflicts_preserve_evidence() {
    legacy_binding_conflict(
        "legacy-owner-conflict",
        "ownerId",
        serde_json::json!("foreign-owner"),
    )
    .await;
    legacy_binding_conflict(
        "legacy-request-conflict",
        "requestId",
        serde_json::json!("foreign-request"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_binding_scope_and_cleanup_conflicts_reconcile_original_request() {
    for (field, value) in [
        ("cleanupPlanDigest", serde_json::json!("0".repeat(64))),
        ("sessionId", serde_json::json!(Uuid::new_v4())),
        ("workspaceDigest", serde_json::json!("0".repeat(64))),
        ("tool", serde_json::json!("foreign-tool")),
        ("payloadHash", serde_json::json!("foreign-payload")),
    ] {
        legacy_binding_conflict(&format!("legacy-{field}-conflict"), field, value).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_admission_recorded_prior_beside_installed_next_recovers() {
    for cut in [3, 4] {
        let _reset = ResetFault;
        let (harness, work_id, digest, revision) =
            approved_repair(&format!("legacy-partial-{cut}")).await;
        let kept = keep_runtime(&harness);
        let request_id = format!("legacy-partial-{cut}-request");
        ADMISSION_FAULT.store(cut, std::sync::atomic::Ordering::SeqCst);
        apply_at(&harness, &request_id, &work_id, &digest, revision)
            .await
            .unwrap_err();
        ADMISSION_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
        let root = harness.orch.store().root().to_path_buf();
        let journal_path = only_json(&root.join("apply-admissions"));
        let journal: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        let modern = primary_receipt(&root, &request_id).0;
        let legacy = modern
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(modern.file_name().unwrap());
        // Migration may leave its exact prior copy after the modern seal write.
        // The durable journal authenticates that one cleanup-only transition.
        fs::write(&legacy, journal["receiptPriorText"].as_str().unwrap()).unwrap();
        let source = source_at(&kept.workspace);
        let (live, _parked) = reopen_kept(harness, &kept).await;
        assert!(
            !legacy.exists(),
            "cut {cut}: recorded prior copy did not converge"
        );
        assert!(!journal_path.exists());
        assert!(intent_files(&root).is_empty());
        let work = live.orch.store().load_work_item(&work_id).unwrap().unwrap();
        assert!(
            !work
                .result
                .as_ref()
                .unwrap()
                .candidate_verification
                .as_ref()
                .unwrap()
                .reconciliation_required
        );
        let replay = live
            .orch
            .apply_verified_change(
                &auth(),
                &request_id,
                kept.lane,
                &kept.workspace,
                &work_id,
                &digest,
                Some(revision),
            )
            .await
            .unwrap_err();
        assert!(
            replay.to_string().contains("definitely not applied"),
            "{replay}"
        );
        let bytes = fs::read(&modern).unwrap();
        let again = reopen_production_store(
            live.host,
            live.orch,
            &kept.workspace,
            &kept.fake,
            &kept.isolate,
            &kept.identity,
            &kept.lease,
        )
        .await;
        assert_eq!(fs::read(&modern).unwrap(), bytes);
        assert_eq!(source_at(&kept.workspace), source);
        stop_live(again).await;
    }
}
