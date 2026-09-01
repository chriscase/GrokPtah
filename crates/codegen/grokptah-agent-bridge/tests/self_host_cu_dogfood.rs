//! Bounded self-host Computer Use dogfood on the deterministic simulator.
//!
//! Composes existing host seams only: a manager plan with a host-owned
//! Computer Use step, then a dependent isolated-review managed Grok step that
//! cannot materialize until that work is `WorkState::Succeeded`. Nothing here
//! opens an application, requests a macOS permission, launches a VM, or calls
//! a provider.

#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use grokptah_agent_bridge::computer_use::{
    canonical_workspace_string, project_run_at, ActionClass, ActionGrant, ActionOutcome,
    AdaptiveClaim, AdaptiveDisposition, AdaptiveProfile, AdaptiveReason, AmbiguityAssessment,
    ComputerAction, ComputerBackend, ComputerCapabilities, ComputerControlDisposition,
    ComputerError, ComputerErrorCode, ComputerObservation, ComputerReadBinding, ComputerRun,
    ComputerRunReads, ComputerRunState, ComputerStore, ComputerUseLimits, GrantIssuer,
    SimulatorBackend,
};
use grokptah_agent_bridge::orchestration::{
    AuthContext, ManagedExecutionBudgetProfile, ManagedExecutionPolicy, ManagedExecutorKind,
    ManagedGrokExecutorConfig, ManagerStepSpec, OrchErrorCode, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, WorkItem, WorkPolicy, WorkResult, WorkRetryPolicy, WorkState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, ComputerUseService,
    CredentialLeaseHandle, CredentialLeaseResolver, GrokBuildAdapterError, HostConfig, SessionKind,
};
use grokptah_agent_sdk::GrokBuildGitIdentity;
use reqwest::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use common::ProcessEnvGuard;

/// Exercise the public MCP projection path without conflating it with the
/// intentionally local-only Computer Run mutation boundary. The public
/// control plane owns scoped reads; the canonical ComputerUseService owns
/// create/authorize/observe/act/complete.
async fn public_mcp_tool(
    client: &reqwest::Client,
    url: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> (StatusCode, Value) {
    let response = client
        .post(url)
        .header("Authorization", "Bearer managed-cu-dogfood-token")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .send()
        .await
        .expect("public MCP request");
    let status = response.status();
    (
        status,
        response.json().await.expect("public MCP JSON response"),
    )
}

/// Simulator-backed backend that counts dispatches, matching the adaptive-seam fixture.
#[derive(Debug)]
struct DogfoodBackend {
    inner: SimulatorBackend,
    actions: AtomicUsize,
}

impl DogfoodBackend {
    fn new() -> Self {
        Self {
            inner: SimulatorBackend::new(),
            actions: AtomicUsize::new(0),
        }
    }

    fn action_calls(&self) -> usize {
        self.actions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ComputerBackend for DogfoodBackend {
    fn capabilities(&self) -> ComputerCapabilities {
        self.inner.capabilities()
    }

    async fn observe(
        &self,
        run_id: &str,
        observation_id: &str,
        target: &grokptah_agent_bridge::computer_use::ComputerTarget,
        limits: &ComputerUseLimits,
    ) -> Result<ComputerObservation, ComputerError> {
        self.inner
            .observe(run_id, observation_id, target, limits)
            .await
    }

    async fn act(
        &self,
        run_id: &str,
        observation: &ComputerObservation,
        action: &ComputerAction,
    ) -> Result<ActionOutcome, ComputerError> {
        self.actions.fetch_add(1, Ordering::SeqCst);
        self.inner.act(run_id, observation, action).await
    }

    async fn cancel(&self, run_id: &str) -> Result<(), ComputerError> {
        self.inner.cancel(run_id).await
    }
}

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
        repository_id: "repo-grokptah-cu-dogfood".into(),
        git_ref: "refs/heads/topic".into(),
        base_sha: head.clone(),
        head_sha: head,
    }
}

fn install_fake_grok(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, FAKE_GROK).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn grok_budget(profile: AdaptiveProfile) -> ManagedExecutionBudgetProfile {
    match profile {
        AdaptiveProfile::Economy => ManagedExecutionBudgetProfile::Economy,
        AdaptiveProfile::HighAssurance => ManagedExecutionBudgetProfile::HighAssurance,
        AdaptiveProfile::Balanced => ManagedExecutionBudgetProfile::Balanced,
    }
}

fn grok_policy(profile: AdaptiveProfile) -> ManagedExecutionPolicy {
    let budget = grok_budget(profile);
    ManagedExecutionPolicy {
        enabled: true,
        allowed_work_kinds: vec!["isolated-review".into()],
        max_concurrent_runs: 1,
        bounds: RunBounds {
            max_prompt_bytes: budget.limits().max_prompt_bytes,
            max_rounds: budget.limits().max_turns,
            max_duration_ms: budget.limits().max_duration_ms,
            max_total_tokens: Some(16_000),
        },
        retry_eligible: false,
        requires_approval_before_execution: true,
        executor: ManagedExecutorKind::GrokBuildIsolatedReview,
        budget_profile: Some(budget),
        ..ManagedExecutionPolicy::default()
    }
}

fn auth() -> AuthContext {
    AuthContext {
        token_id: "operator".into(),
        owner_id: "primary".into(),
    }
}

fn host_cu_step() -> ManagerStepSpec {
    ManagerStepSpec {
        step_id: "host-cu".into(),
        kind: "host-computer-use".into(),
        objective: "Host-owned simulator Computer Use before managed Grok.".into(),
        priority: 0,
        dependencies: Vec::new(),
        assigned_agent_id: None,
        policy: WorkPolicy {
            retry: WorkRetryPolicy {
                max_attempts: 1,
                retry_failed: false,
                retry_expired: false,
                backoff_ms: 0,
            },
            requires_approval: true,
            ..WorkPolicy::default()
        },
    }
}

fn grok_step(agent_id: &str) -> ManagerStepSpec {
    ManagerStepSpec {
        step_id: "isolated-review".into(),
        kind: "isolated-review".into(),
        objective: "Open DOGFOOD.txt and replace its exact contents `before\\n` with `after\\n`. Edit no other file. Inspect the final diff and report truthfully.".into(),
        priority: 0,
        dependencies: vec!["host-cu".into()],
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

fn grant(run: &ComputerRun) -> ActionGrant {
    let now = Utc::now();
    ActionGrant {
        grant_id: format!("grant-{}", Uuid::new_v4()),
        run_id: run.run_id.clone(),
        target: run.target.clone(),
        action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        issued_by: GrantIssuer::LocalUser,
        issued_at: now - Duration::seconds(1),
        expires_at: now + Duration::minutes(5),
        uses_remaining: Some(8),
        revoked_at: None,
    }
}

fn claim(profile: AdaptiveProfile, run: &ComputerRun, sequence: u64) -> AdaptiveClaim {
    AdaptiveClaim {
        profile,
        planner: AdaptiveDisposition::Commit,
        assessment: AmbiguityAssessment::unambiguous(9_600),
        observed_control_epoch: run.control_epoch,
        observed_sequence: sequence,
        approval: None,
    }
}

fn set_name(observation: &ComputerObservation) -> ComputerAction {
    ComputerAction::SetValue {
        element_id: format!("{}-name", observation.observation_id),
        text: "Ada".into(),
    }
}

fn adaptive_of(
    service: &ComputerUseService,
    run_id: &str,
) -> Option<grokptah_agent_bridge::computer_use::AdaptiveDecisionSummary> {
    let run = service
        .get_run(run_id)
        .expect("run readable")
        .expect("run exists");
    project_run_at(&run, Utc::now()).adaptive
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
    assert!(!result.summary.contains(&workspace_text));
    assert!(!result.summary.contains("/private/") && !result.summary.contains("/var/"));
    for artifact in &result.artifacts {
        let artifact_text = format!("{artifact:?}");
        assert!(!artifact_text.contains("opaque-test-credential"));
        assert!(!artifact_text.contains(&workspace_text));
        assert!(!artifact_text.contains("/private/") && !artifact_text.contains("/var/"));
    }
    if let Some(failure) = &result.failure {
        assert!(!failure.contains("opaque-test-credential"));
        assert!(!failure.contains(&workspace_text));
        assert!(!failure.contains("/private/") && !failure.contains("/var/"));
    }
}

/// Serialize the real host projection and require it not to carry observation
/// values, credentials, workspace paths, or grant payloads. The raw run is
/// checked first so a missing fixture value cannot make the projection check
/// vacuously pass.
fn assert_redacted_cu_projection(service: &ComputerUseService, run_id: &str, workspace: &Path) {
    let run = service
        .get_run(run_id)
        .expect("run readable")
        .expect("run exists");
    let raw = serde_json::to_string(&run).expect("raw run json");
    let projected =
        serde_json::to_string(&project_run_at(&run, Utc::now())).expect("projection json");
    let workspace_text = workspace.display().to_string();

    if let Some(observation) = &run.current_observation {
        let name_id = format!("{}-name", observation.observation_id);
        assert!(
            raw.contains(&name_id),
            "raw run lost the fixture element id the projection must hide"
        );
        assert!(
            raw.contains("Not submitted") || raw.contains("\"Name\""),
            "raw run lost fixture observation labels the projection must hide"
        );
        assert!(
            !projected.contains(&name_id),
            "projection leaked observation element id: {projected}"
        );
        assert!(
            !projected.contains("Not submitted"),
            "projection leaked observation value: {projected}"
        );
        assert!(
            !projected.contains("\"Name\""),
            "projection leaked observation label: {projected}"
        );
    }
    if let Some(outcome) = &run.last_outcome {
        assert!(
            raw.contains(&outcome.summary),
            "raw run lost the action summary the projection must hide"
        );
        assert!(
            !projected.contains(&outcome.summary),
            "projection leaked action summary: {projected}"
        );
    }
    if let Some(grant) = &run.grant {
        assert!(
            raw.contains(&grant.grant_id),
            "raw run lost the grant id used as a leak oracle"
        );
        assert!(
            raw.contains(&grant.target.display_name),
            "raw grant lost target detail the projection grant must not copy as payload"
        );
        let grant_projection = serde_json::to_value(project_run_at(&run, Utc::now()))
            .expect("projection value")
            .get("grant")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        assert!(
            grant_projection.get("target").is_none(),
            "projection copied the raw grant target payload: {grant_projection}"
        );
    }
    if let Some(bound) = &run.workspace {
        assert!(
            raw.contains(bound),
            "raw run lost the workspace binding the projection must hide"
        );
        assert!(
            !projected.contains(bound),
            "projection leaked workspace binding: {projected}"
        );
    }
    assert!(
        !projected.contains(&workspace_text),
        "projection leaked workspace path: {projected}"
    );
    assert!(
        !projected.contains("opaque-test-credential"),
        "projection leaked credential material: {projected}"
    );
    assert!(
        !projected.contains("/private/") && !projected.contains("/var/"),
        "projection leaked an absolute path: {projected}"
    );
}

fn assert_opaque_cu_run_evidence(result: &WorkResult, original: &str, successor: &str) {
    assert_eq!(
        result.evidence,
        [
            format!("computer_run:{original}"),
            format!("computer_run:{successor}"),
        ]
    );
    let rendered = format!("{result:?}");
    assert!(!rendered.contains("opaque-test-credential"));
    assert!(!result.summary.contains("opaque-test-credential"));
    for entry in &result.evidence {
        assert!(
            entry.starts_with("computer_run:")
                && !entry.contains("grant-")
                && !entry.contains("Ada"),
            "CU work evidence is not an opaque run id: {entry}"
        );
    }
}

async fn observe(
    service: &ComputerUseService,
    run: &ComputerRun,
    request_id: &str,
) -> (ComputerRun, ComputerObservation) {
    let observation = service
        .observe(request_id, &run.run_id, run.version)
        .await
        .expect("observe");
    let run = service
        .get_run(&run.run_id)
        .expect("run readable")
        .expect("run exists");
    (run, observation)
}

async fn drive_until_review_gate(
    orch: &OrchestrationService,
    work_id: &str,
    timeout_secs: u64,
) -> grokptah_agent_bridge::orchestration::WorkItem {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
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

/// Explicit inputs for the opt-in live composition gate.  The normal test
/// path never reads these variables and continues to use the deterministic
/// fake CLI.
#[derive(Debug)]
struct LiveGate {
    executable: PathBuf,
    credential_source: PathBuf,
    candidate_head: String,
    evidence_dir: PathBuf,
}

impl LiveGate {
    fn from_env() -> Self {
        Self {
            executable: PathBuf::from(
                std::env::var_os("GROKPTAH_LIVE_GROK")
                    .expect("GROKPTAH_LIVE_GROK must name the authorized Grok executable"),
            ),
            credential_source: PathBuf::from(
                std::env::var_os("GROKPTAH_LIVE_GROK_AUTH")
                    .expect("GROKPTAH_LIVE_GROK_AUTH must name the authorized credential file"),
            ),
            candidate_head: std::env::var("GROKPTAH_LIVE_CANDIDATE_HEAD")
                .expect("GROKPTAH_LIVE_CANDIDATE_HEAD must bind the qualification source"),
            evidence_dir: PathBuf::from(
                std::env::var_os("GROKPTAH_LIVE_EVIDENCE_DIR").expect(
                    "GROKPTAH_LIVE_EVIDENCE_DIR must name the secret-free evidence directory",
                ),
            ),
        }
    }
}

fn profile_name(profile: AdaptiveProfile) -> &'static str {
    match profile {
        AdaptiveProfile::Economy => "economy",
        AdaptiveProfile::Balanced => "balanced",
        AdaptiveProfile::HighAssurance => "high_assurance",
    }
}

async fn prove_adaptive_invariants(
    service: &ComputerUseService,
    backend: &DogfoodBackend,
    run: &ComputerRun,
    observation: &ComputerObservation,
    profile: AdaptiveProfile,
) {
    let observation_id = observation.observation_id.clone();
    let below_high = AdaptiveClaim {
        assessment: AmbiguityAssessment::unambiguous(7_000),
        ..claim(AdaptiveProfile::HighAssurance, run, observation.sequence)
    };
    let error = service
        .act_with_plan(
            "act-high-below-floor",
            &run.run_id,
            run.version,
            &observation_id,
            set_name(observation),
            below_high,
        )
        .await
        .expect_err("HighAssurance below-floor must refuse");
    assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
    assert_eq!(backend.action_calls(), 0);
    let summary = adaptive_of(service, &run.run_id).expect("refusal recorded");
    assert!(!summary.admitted);
    assert_eq!(summary.profile, AdaptiveProfile::HighAssurance);
    assert_eq!(summary.reason, AdaptiveReason::ConfidenceBelowFloor);

    let below_economy = AdaptiveClaim {
        assessment: AmbiguityAssessment::unambiguous(5_000),
        ..claim(AdaptiveProfile::Economy, run, observation.sequence)
    };
    let error = service
        .act_with_plan(
            "act-economy-approval",
            &run.run_id,
            run.version,
            &observation_id,
            set_name(observation),
            below_economy.clone(),
        )
        .await
        .expect_err("Economy below-floor requires approval rather than dispatch");
    assert_eq!(error.code, ComputerErrorCode::PermissionRequired);
    assert_eq!(backend.action_calls(), 0);
    let summary = adaptive_of(service, &run.run_id).expect("approval gate recorded");
    assert!(!summary.admitted);
    assert_eq!(summary.reason, AdaptiveReason::ApprovalRequired);

    let mut forged = serde_json::to_value(&below_economy).expect("claim serializes");
    forged["approval"] = serde_json::json!({
        "runId": run.run_id,
        "controlEpoch": run.control_epoch,
        "observationId": observation_id,
        "approved": true,
        "binding": "0".repeat(64),
    });
    assert!(serde_json::from_value::<AdaptiveClaim>(forged).is_err());

    let mismatch = service
        .act_with_plan(
            "act-economy-approval",
            &run.run_id,
            run.version,
            &observation_id,
            set_name(observation),
            claim(profile, run, observation.sequence),
        )
        .await
        .expect_err("request-id payload mismatch must fail closed");
    assert_eq!(mismatch.code, ComputerErrorCode::Conflict);
    assert_eq!(backend.action_calls(), 0);
}

async fn host_cu_runs(
    computer_root: &Path,
    lane_id: Uuid,
    workspace: &Path,
    profile: AdaptiveProfile,
) -> (
    String,
    String,
    serde_json::Value,
    serde_json::Value,
    serde_json::Value,
    serde_json::Value,
) {
    let binding = canonical_workspace_string(workspace).expect("canonical workspace");
    let backend = Arc::new(DogfoodBackend::new());
    let store = ComputerStore::open(computer_root).expect("open cu store");
    let reads = ComputerRunReads::new(store.clone());
    let service = ComputerUseService::new(backend.clone(), store);

    let original = service
        .create_run(
            "cu-create-original",
            lane_id,
            Some(binding.clone()),
            SimulatorBackend::demo_target(),
            ComputerUseLimits::default(),
        )
        .expect("create original run");
    let original_grant = grant(&original);
    let original = service
        .authorize(
            "cu-authorize-original",
            &original.run_id,
            original.version,
            original_grant.clone(),
        )
        .expect("authorize original");
    let (original, observation) = observe(&service, &original, "cu-observe-original").await;
    assert!(original.grant.is_some());
    assert!(original.current_observation.is_some());
    assert_redacted_cu_projection(&service, &original.run_id, workspace);
    let original_live_projection =
        serde_json::to_value(project_run_at(&original, Utc::now())).expect("live projection");

    let now = Utc::now();
    let owner = ComputerReadBinding::new(lane_id, &binding);
    assert!(reads.project_run(owner, &original.run_id, now).is_ok());
    let other_lane = Uuid::new_v4();
    let cross_lane = reads
        .project_run(
            ComputerReadBinding::new(other_lane, &binding),
            &original.run_id,
            now,
        )
        .expect_err("cross-lane CU read");
    let unknown = reads
        .project_run(
            ComputerReadBinding::new(other_lane, &binding),
            "no-such-run",
            now,
        )
        .expect_err("unknown CU read");
    assert_eq!(cross_lane.code, ComputerErrorCode::Unauthorized);
    assert_eq!(cross_lane, unknown);
    let cross_workspace = reads
        .project_run(
            ComputerReadBinding::new(lane_id, "/tmp/grokptah-cu-dogfood-foreign"),
            &original.run_id,
            now,
        )
        .expect_err("cross-workspace CU read");
    assert_eq!(cross_workspace, cross_lane);
    let session_cross = service
        .project_session_run(other_lane, &original.run_id, now)
        .expect_err("session-scoped cross-lane");
    assert_eq!(session_cross.code, ComputerErrorCode::Unauthorized);

    prove_adaptive_invariants(&service, &backend, &original, &observation, profile).await;
    let original_id = original.run_id.clone();
    drop(reads);
    drop(service);
    drop(backend);

    let backend = Arc::new(DogfoodBackend::new());
    let store = ComputerStore::open(computer_root).expect("reopen cu store");
    let service = ComputerUseService::new(backend.clone(), store);
    let recovered = service
        .project_session_run(lane_id, &original_id, Utc::now())
        .expect("recovered original");
    assert_eq!(recovered.state, ComputerRunState::Interrupted);
    assert_eq!(
        recovered.control_disposition,
        ComputerControlDisposition::Interrupted
    );
    assert!(recovered.grant.is_none());
    assert!(recovered.observation.is_none());
    let recovered_run = service
        .get_run(&original_id)
        .expect("load recovered")
        .expect("recovered exists");
    assert!(recovered_run.grant.is_none());
    assert!(recovered_run.current_observation.is_none());
    let recovered_projection = serde_json::to_string(&project_run_at(&recovered_run, Utc::now()))
        .expect("recovered projection json");
    assert!(!recovered_projection.contains("opaque-test-credential"));
    assert!(!recovered_projection.contains(&workspace.display().to_string()));

    let successor = service
        .create_run(
            "cu-create-successor",
            lane_id,
            Some(binding),
            SimulatorBackend::demo_target(),
            ComputerUseLimits::default(),
        )
        .expect("create successor");
    assert_ne!(
        original_id, successor.run_id,
        "restart must mint a new CU run"
    );
    let successor_grant = grant(&successor);
    assert_ne!(successor_grant.grant_id, original_grant.grant_id);
    let successor = service
        .authorize(
            "cu-authorize-successor",
            &successor.run_id,
            successor.version,
            successor_grant,
        )
        .expect("authorize successor");
    let (successor, observation) = observe(&service, &successor, "cu-observe-successor").await;
    assert_redacted_cu_projection(&service, &successor.run_id, workspace);
    let successor_live_projection =
        serde_json::to_value(project_run_at(&successor, Utc::now())).expect("live projection");
    let observation_id = observation.observation_id.clone();
    service
        .act_with_plan(
            "cu-act-successor",
            &successor.run_id,
            successor.version,
            &observation_id,
            set_name(&observation),
            claim(profile, &successor, observation.sequence),
        )
        .await
        .unwrap_or_else(|error| panic!("{profile:?} refused a clean successor action: {error}"));
    assert_eq!(backend.action_calls(), 1);
    assert_redacted_cu_projection(&service, &successor.run_id, workspace);
    let summary = adaptive_of(&service, &successor.run_id).expect("admitted review");
    assert!(summary.admitted);
    assert_eq!(summary.profile, profile);
    assert_eq!(summary.reason, AdaptiveReason::Admitted);
    let replay = service
        .act_with_plan(
            "cu-act-successor",
            &successor.run_id,
            successor.version,
            &observation_id,
            set_name(&observation),
            claim(
                if profile == AdaptiveProfile::Economy {
                    AdaptiveProfile::HighAssurance
                } else {
                    AdaptiveProfile::Economy
                },
                &successor,
                observation.sequence,
            ),
        )
        .await
        .expect_err("successor request-id payload mismatch");
    assert_eq!(replay.code, ComputerErrorCode::Conflict);
    assert_eq!(backend.action_calls(), 1);

    let successor = service
        .get_run(&successor.run_id)
        .expect("load successor")
        .expect("successor exists");
    let completed = service
        .complete(
            "cu-complete-successor",
            &successor.run_id,
            successor.version,
        )
        .expect("complete successor");
    assert_eq!(completed.state, ComputerRunState::Completed);
    let original_projection = serde_json::to_value(project_run_at(&recovered_run, Utc::now()))
        .expect("interrupted Computer Run projection");
    let successor_projection =
        serde_json::to_value(project_run_at(&completed, Utc::now())).expect("successor projection");
    (
        original_id,
        completed.run_id,
        original_live_projection,
        successor_live_projection,
        original_projection,
        successor_projection,
    )
}

#[cfg(unix)]
async fn run_profile(
    profile: AdaptiveProfile,
    retain_managed_result: bool,
    live: Option<&LiveGate>,
) {
    let mut env = ProcessEnvGuard::new();
    let home = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");

    let workspace = tempfile::tempdir().unwrap();
    let identity = initialize_repo(workspace.path());
    let isolate_parent = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(isolate_parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let fake_dir = tempfile::tempdir().unwrap();
    let fake_grok = fake_dir.path().join("grok");
    let lease_path = fake_dir.path().join("lease.json");
    let executable = if let Some(gate) = live {
        fs::copy(&gate.credential_source, &lease_path)
            .expect("copy disposable live Grok credential lease");
        gate.executable.clone()
    } else {
        install_fake_grok(&fake_grok);
        fs::write(&lease_path, b"opaque-test-credential\n").unwrap();
        fake_grok.clone()
    };
    assert!(
        executable.is_file(),
        "managed Grok executable is unavailable"
    );
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600)).unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire GrokPtah instance lock");
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let other_lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other_lane.id, workspace.path())
        .unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "managed-cu-dogfood-token".into(),
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
            identity: identity.clone(),
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

    let mut cu_worker = orch
        .store()
        .load_agent(&agent.agent_id)
        .unwrap()
        .expect("session agent");
    cu_worker.agent_id = "cu-worker".into();
    if let Some(spec) = cu_worker.spec.as_mut() {
        spec.authority.computer_use_allowed = true;
    }
    orch.store().save_agent(&cu_worker).unwrap();
    let mut forbidden_step = host_cu_step();
    forbidden_step.assigned_agent_id = Some("cu-worker".into());
    let cu_worker_plan = orch
        .create_manager_plan(
            &auth(),
            "cu-worker-plan",
            lane.id,
            workspace.path(),
            agent.agent_id.clone(),
            "must not assign a Computer Use worker".into(),
            vec![forbidden_step],
            1,
            1,
            false,
        )
        .await
        .expect_err("Computer Use workers are refused");
    assert_eq!(cu_worker_plan.code, OrchErrorCode::ForbiddenScope);

    let created = orch
        .create_manager_plan(
            &auth(),
            "cu-dogfood-create",
            lane.id,
            workspace.path(),
            agent.agent_id.clone(),
            "Host CU then isolated review".into(),
            vec![host_cu_step(), grok_step(&agent.agent_id)],
            1,
            1,
            false,
        )
        .await
        .expect("create manager plan");
    let plan_id = created["plan"]["planId"]
        .as_str()
        .expect("plan id")
        .to_string();
    let revision = created["plan"]["revision"].as_u64().expect("plan revision");
    let advanced = orch
        .advance_manager_plan(
            &auth(),
            "cu-dogfood-advance-1",
            lane.id,
            workspace.path(),
            &plan_id,
            Some(revision),
        )
        .await
        .expect("advance host CU step");
    assert_eq!(advanced["createdWork"].as_array().map(Vec::len), Some(1));
    let cu_plan_revision = advanced["plan"]["revision"]
        .as_u64()
        .expect("cu plan revision");
    let cu_work_id = advanced["createdWork"][0]["workId"]
        .as_str()
        .expect("cu work id")
        .to_string();
    let cu_work = orch.store().load_work_item(&cu_work_id).unwrap().unwrap();
    assert_eq!(cu_work.kind, "host-computer-use");
    assert_eq!(cu_work.source_manager_step_id.as_deref(), Some("host-cu"));
    assert_ne!(cu_work.state, WorkState::Succeeded);
    let plan = orch
        .get_manager_plan_scoped(&auth(), lane.id, workspace.path(), &plan_id)
        .unwrap();
    assert!(plan["plan"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["stepId"] == "isolated-review")
        .unwrap()["workId"]
        .is_null());
    orch.drive_native_executor_once().await;
    assert!(orch.store().list_managed_intents().unwrap().is_empty());

    let foreign = tempfile::tempdir().unwrap();
    let cross_workspace = orch
        .advance_manager_plan(
            &auth(),
            "cu-dogfood-cross-workspace",
            lane.id,
            foreign.path(),
            &plan_id,
            Some(
                advanced["plan"]["revision"]
                    .as_u64()
                    .expect("advanced plan revision"),
            ),
        )
        .await
        .expect_err("cross-workspace plan mutation");
    assert_eq!(cross_workspace.code, OrchErrorCode::WorkspaceMismatch);
    let cross_lane = orch
        .claim_work(
            &auth(),
            "cu-dogfood-cross-lane-claim",
            other_lane.id,
            workspace.path(),
            &cu_work_id,
            Some(60_000),
            None,
        )
        .await
        .expect_err("cross-lane work claim");
    let unknown_claim = orch
        .claim_work(
            &auth(),
            "cu-dogfood-unknown-claim",
            other_lane.id,
            workspace.path(),
            "no-such-work",
            Some(60_000),
            None,
        )
        .await
        .expect_err("unknown work claim");
    assert_eq!(cross_lane.code, unknown_claim.code);
    assert_eq!(cross_lane.to_string(), unknown_claim.to_string());

    let claimed = orch
        .claim_work(
            &auth(),
            "cu-dogfood-claim",
            lane.id,
            workspace.path(),
            &cu_work_id,
            Some(60_000),
            None,
        )
        .await
        .expect("claim host CU work");
    let attempt_id = claimed["attempt"]["attemptId"]
        .as_str()
        .expect("attempt id")
        .to_string();
    let lease_token = claimed["leaseToken"]
        .as_str()
        .expect("lease token")
        .to_string();

    let computer_root = host.runtime_home().computer_root();

    let (
        original_run_id,
        successor_run_id,
        original_live_projection,
        successor_live_projection,
        original_projection,
        successor_projection,
    ) = host_cu_runs(&computer_root, lane.id, workspace.path(), profile).await;
    let completed = orch
        .complete_work(
            &auth(),
            "cu-dogfood-complete",
            lane.id,
            workspace.path(),
            &cu_work_id,
            &attempt_id,
            &lease_token,
            WorkResult {
                summary: "host computer use succeeded".into(),
                evidence: vec![
                    format!("computer_run:{original_run_id}"),
                    format!("computer_run:{successor_run_id}"),
                ],
                artifacts: Vec::new(),
                failure: None,
                cancellation_reason: None,
                completed_at: Utc::now(),
                verification: None,
            },
        )
        .await
        .expect("complete host CU work");
    assert_eq!(completed["work"]["state"], "awaiting_approval");
    let cu_work = orch.store().load_work_item(&cu_work_id).unwrap().unwrap();
    assert_opaque_cu_run_evidence(
        cu_work.result.as_ref().expect("cu result"),
        &original_run_id,
        &successor_run_id,
    );
    let blocked = orch
        .advance_manager_plan(
            &auth(),
            "cu-dogfood-advance-blocked",
            lane.id,
            workspace.path(),
            &plan_id,
            Some(cu_plan_revision),
        )
        .await
        .expect("advance while CU is awaiting approval");
    assert!(
        blocked["createdWork"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true),
        "Grok work materialized before CU Succeeded"
    );
    let approved = orch
        .approve_work(
            &auth(),
            "cu-dogfood-approve",
            lane.id,
            workspace.path(),
            &cu_work_id,
            Some("host CU evidence reviewed".into()),
            Some(cu_work.revision),
        )
        .await
        .expect("approve host CU work");
    assert_eq!(approved["work"]["state"], "succeeded");
    let cu_work = orch.store().load_work_item(&cu_work_id).unwrap().unwrap();
    assert_eq!(cu_work.state, WorkState::Succeeded);
    assert_opaque_cu_run_evidence(
        cu_work.result.as_ref().expect("cu result"),
        &original_run_id,
        &successor_run_id,
    );

    let plan = orch
        .get_manager_plan_scoped(&auth(), lane.id, workspace.path(), &plan_id)
        .unwrap();
    let plan_revision = plan["plan"]["revision"].as_u64().expect("plan revision");
    let grok_advanced = orch
        .advance_manager_plan(
            &auth(),
            "cu-dogfood-advance-2",
            lane.id,
            workspace.path(),
            &plan_id,
            Some(plan_revision),
        )
        .await
        .expect("advance dependent Grok step");
    assert_eq!(
        grok_advanced["createdWork"].as_array().map(Vec::len),
        Some(1)
    );
    let grok_work_id = grok_advanced["createdWork"][0]["workId"]
        .as_str()
        .expect("grok work id")
        .to_string();
    let grok_work = orch.store().load_work_item(&grok_work_id).unwrap().unwrap();
    assert_eq!(grok_work.kind, "isolated-review");
    assert_eq!(
        grok_work.assigned_agent_id.as_deref(),
        Some(agent.agent_id.as_str())
    );
    assert_eq!(grok_work.dependencies.len(), 1);
    assert_eq!(grok_work.dependencies[0].work_id, cu_work_id);
    assert_eq!(
        grok_work.dependencies[0].required_state,
        WorkState::Succeeded
    );
    assert_ne!(grok_work.state, WorkState::Succeeded);

    let authorized = orch
        .authorize_work_execution(
            &auth(),
            "cu-dogfood-authorize-grok",
            lane.id,
            workspace.path(),
            &grok_work_id,
            "authorize dependent isolated review".into(),
            Some(grok_work.revision),
        )
        .await
        .expect("authorize grok execution");
    let _ = authorized;
    let reviewed =
        drive_until_review_gate(&orch, &grok_work_id, if live.is_some() { 180 } else { 10 }).await;
    assert_eq!(
        reviewed.state,
        WorkState::AwaitingApproval,
        "managed executor finalized unexpectedly: {:?}",
        reviewed.result
    );
    assert_ne!(reviewed.state, WorkState::Succeeded);
    assert_redacted_operator_result(&reviewed, workspace.path());
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
    assert_eq!(
        fs::read_to_string(workspace.path().join("DOGFOOD.txt")).unwrap(),
        "after\n",
        "live managed result did not apply the bounded edit: {:?}",
        reviewed.result
    );

    // The operator-facing handoff is assembled only from the redacted graph
    // and Computer Run projections. Keep this oracle next to the composed
    // journey so a future report cannot accidentally switch to raw WorkItem
    // or ComputerRun records that carry objectives, paths, grants, or
    // observations. The graph method is the same public read seam used by
    // ptah_get_work_graph, and the run values use the same projection helper
    // used by the ptah_get_computer_run path. Computer Run mutations
    // intentionally remain local-only, per the control-plane contract; this
    // test proves the shared public projection shape, not HTTP dispatch.
    let graph = orch
        .get_work_graph_scoped(&auth(), lane.id, workspace.path())
        .expect("redacted work graph");
    let graph_nodes = graph["graph"].as_array().expect("graph nodes");
    assert!(graph_nodes
        .iter()
        .any(|node| { node["workId"] == cu_work_id && node["state"] == "succeeded" }));
    assert!(graph_nodes
        .iter()
        .any(|node| { node["workId"] == grok_work_id && node["state"] == "awaiting_approval" }));
    let public_report = serde_json::json!({
        "profile": profile,
        "workGraph": graph["graph"].clone(),
        "computerRuns": [
            original_live_projection,
            successor_live_projection,
            original_projection,
            successor_projection
        ],
    });
    let report_text = serde_json::to_string(&public_report).expect("public report json");
    for secret in [
        "opaque-test-credential",
        lease_token.as_str(),
        workspace.path().to_str().unwrap(),
        "currentObservation",
        "objective",
        "assignedAgentId",
        "leaseToken",
        "Not submitted",
        "\"Name\"",
        "-name",
    ] {
        assert!(
            !report_text.contains(secret),
            "public self-host report leaked {secret}: {report_text}"
        );
    }
    assert!(report_text.contains(&original_run_id));
    assert!(report_text.contains(&successor_run_id));
    assert!(
        report_text.contains("observation-ref-"),
        "live report omitted the safe observation surrogate: {report_text}"
    );
    assert!(!report_text.contains("DOGFOOD.txt"));

    // The report above is assembled from the same projections used by the
    // public MCP reads. The canonical CU runs live in the host-owned ledger,
    // so the control plane below reads the original interrupted run and the
    // successor that was explicitly reauthorized and completed.
    let public_server = start_control_server(orch.clone(), 0)
        .await
        .expect("public MCP control server");
    let public_url = format!("http://{}/mcp", public_server.addr);
    let public_client = reqwest::Client::new();
    let (status, listed) = public_mcp_tool(
        &public_client,
        &public_url,
        200,
        "ptah_list_computer_runs",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": workspace.path(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let listed_runs = listed["result"]["structuredContent"]["runs"]
        .as_array()
        .expect("public MCP run list");
    assert_eq!(listed_runs.len(), 2);
    let listed_ids: BTreeSet<&str> = listed_runs
        .iter()
        .filter_map(|run| run["runId"].as_str())
        .collect();
    assert!(listed_ids.contains(original_run_id.as_str()));
    assert!(listed_ids.contains(successor_run_id.as_str()));
    for listed_projection in listed_runs {
        assert!(
            listed_projection["grant"].get("target").is_none(),
            "listed public MCP projection copied a grant target payload: {listed_projection}"
        );
    }
    let listed_text = serde_json::to_string(listed_runs).expect("listed MCP projections");
    for forbidden in [
        workspace.path().to_str().unwrap(),
        "currentObservation",
        "leaseToken",
        "opaque-test-credential",
        lease_token.as_str(),
        "Not submitted",
        "\"Name\"",
        "-name",
    ] {
        assert!(
            !listed_text.contains(forbidden),
            "listed public MCP projections leaked {forbidden}: {listed_text}"
        );
    }

    let (status, interrupted_fetch) = public_mcp_tool(
        &public_client,
        &public_url,
        201,
        "ptah_get_computer_run",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": workspace.path(),
            "run_id": original_run_id,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let interrupted_projection = &interrupted_fetch["result"]["structuredContent"];
    assert_eq!(interrupted_projection["runId"], original_run_id);
    assert_eq!(interrupted_projection["state"], "interrupted");
    assert!(interrupted_projection["grant"].is_null());
    assert!(interrupted_projection["observation"].is_null());
    let interrupted_text =
        serde_json::to_string(interrupted_projection).expect("interrupted MCP projection");
    for forbidden in [
        workspace.path().to_str().unwrap(),
        "currentObservation",
        "leaseToken",
        "opaque-test-credential",
        lease_token.as_str(),
    ] {
        assert!(
            !interrupted_text.contains(forbidden),
            "interrupted public MCP projection leaked {forbidden}: {interrupted_text}"
        );
    }

    let (status, successor_fetch) = public_mcp_tool(
        &public_client,
        &public_url,
        202,
        "ptah_get_computer_run",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": workspace.path(),
            "run_id": successor_run_id,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let successor_projection = &successor_fetch["result"]["structuredContent"];
    assert_eq!(successor_projection["runId"], successor_run_id);
    assert_eq!(successor_projection["state"], "completed");
    assert!(
        successor_projection["grant"].get("target").is_none(),
        "successor public MCP projection copied a grant target payload: {successor_projection}"
    );
    assert!(
        successor_projection["observation"].is_object(),
        "successor MCP projection lost the observed fixture"
    );
    let successor_text =
        serde_json::to_string(successor_projection).expect("successor MCP projection");
    for forbidden in [
        workspace.path().to_str().unwrap(),
        "currentObservation",
        "leaseToken",
        "opaque-test-credential",
        lease_token.as_str(),
        "Not submitted",
        "\"Name\"",
        "-name",
    ] {
        assert!(
            !successor_text.contains(forbidden),
            "successor public MCP projection leaked {forbidden}: {successor_text}"
        );
    }

    let (status, graph_wire) = public_mcp_tool(
        &public_client,
        &public_url,
        203,
        "ptah_get_work_graph",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": workspace.path(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let graph_text = serde_json::to_string(&graph_wire).expect("public MCP work graph");
    assert!(graph_text.contains(&cu_work_id));
    assert!(graph_text.contains(&grok_work_id));
    assert!(!graph_text.contains(workspace.path().to_str().unwrap()));
    assert!(!graph_text.contains(lease_token.as_str()));

    // Scope errors must be indistinguishable on the actual wire, including
    // a same-workspace cross-lane read and an unknown run id. A foreign
    // workspace is rejected by the earlier workspace allowlist gate.
    let (_, cross_lane) = public_mcp_tool(
        &public_client,
        &public_url,
        204,
        "ptah_get_computer_run",
        serde_json::json!({
            "session_id": other_lane.id,
            "workspace": workspace.path(),
            "run_id": successor_run_id,
        }),
    )
    .await;
    let (_, foreign_scope) = public_mcp_tool(
        &public_client,
        &public_url,
        205,
        "ptah_get_computer_run",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": foreign.path(),
            "run_id": successor_run_id,
        }),
    )
    .await;
    let (_, unknown_run) = public_mcp_tool(
        &public_client,
        &public_url,
        206,
        "ptah_get_computer_run",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": workspace.path(),
            "run_id": "no-such-public-run",
        }),
    )
    .await;
    assert_eq!(cross_lane["error"]["data"]["code"], "forbidden_scope");
    // JSON-RPC request ids are intentionally different; compare the complete
    // error object so only the scoped failure vocabulary is required to match.
    assert_eq!(cross_lane["error"], unknown_run["error"]);
    assert_eq!(foreign_scope["error"]["data"]["code"], "workspace_mismatch");

    let (status, cross_lane_list) = public_mcp_tool(
        &public_client,
        &public_url,
        207,
        "ptah_list_computer_runs",
        serde_json::json!({
            "session_id": other_lane.id,
            "workspace": workspace.path(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cross_lane_runs = cross_lane_list["result"]["structuredContent"]["runs"]
        .as_array()
        .expect("cross-lane public MCP run list");
    assert!(
        cross_lane_runs.is_empty(),
        "cross-lane list exposed runs owned by another lane: {cross_lane_list}"
    );
    let (status, foreign_list) = public_mcp_tool(
        &public_client,
        &public_url,
        208,
        "ptah_list_computer_runs",
        serde_json::json!({
            "session_id": lane.id,
            "workspace": foreign.path(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(foreign_list["error"]["data"]["code"], "workspace_mismatch");

    // Exercise both operator outcomes across the two profiles: Economy keeps
    // the bounded proposal via the approval gate, while High Assurance rejects
    // it through the durable Work failure path. Neither outcome can promote
    // code automatically, and the managed executor has already cleaned its
    // disposable checkout before either decision is recorded.
    if retain_managed_result {
        let kept = orch
            .approve_work(
                &auth(),
                "cu-dogfood-approve-grok",
                lane.id,
                workspace.path(),
                &grok_work_id,
                Some("operator retained the reviewed bounded change".into()),
                Some(reviewed.revision),
            )
            .await
            .expect("approve managed Grok result");
        assert_eq!(kept["work"]["state"], "succeeded");
    } else {
        let attempt = orch
            .store()
            .list_work_attempts(Some(&grok_work_id))
            .expect("list managed Grok attempts")
            .into_iter()
            .last()
            .expect("managed Grok attempt");
        let token = attempt.lease_token_for_secret("managed-cu-dogfood-token");
        let rejected = orch
            .fail_work(
                &auth(),
                "cu-dogfood-reject-grok",
                lane.id,
                workspace.path(),
                &grok_work_id,
                &attempt.attempt_id,
                &token,
                WorkResult {
                    summary: "operator rejected the reviewed bounded change".into(),
                    evidence: vec!["operator:rejected".into()],
                    artifacts: Vec::new(),
                    failure: Some("operator rejected proposed change".into()),
                    cancellation_reason: None,
                    completed_at: Utc::now(),
                    verification: None,
                },
            )
            .await
            .expect("reject managed Grok result");
        assert_eq!(rejected["work"]["state"], "failed");
    }

    if let Some(gate) = live {
        let final_grok = orch
            .store()
            .load_work_item(&grok_work_id)
            .unwrap()
            .expect("final managed Grok work");
        let intent = orch
            .store()
            .list_managed_intents()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.work_id == grok_work_id)
            .expect("managed Grok intent");
        let invocation = intent.grok.as_ref().expect("managed Grok invocation");
        let safe_evidence = serde_json::json!({
            "schemaVersion": 1,
            "candidateHead": gate.candidate_head,
            "profile": profile_name(profile),
            "managerPlanId": plan_id,
            "hostComputerUseWorkId": cu_work_id,
            "managedGrokWorkId": grok_work_id,
            "computerRuns": [original_run_id, successor_run_id],
            "dependencyGate": "managed_grok_created_after_host_cu_succeeded",
            "managed": {
                "state": final_grok.state,
                "attemptId": intent.attempt_id,
                "runId": intent.run_id,
                "requestId": invocation.request_id,
                "cliPermissionMode": invocation.cli_permission_mode.as_str(),
                "hostExecutionApproved": invocation.host_execution_approved,
                "finalHeadSha": invocation.final_head_sha,
                "finalRef": invocation.final_ref,
                "finalState": invocation.final_state,
                "verdict": invocation.verdict,
                "changedPaths": invocation.changed_paths,
                "diffDigest": invocation.diff_digest,
                "evidenceRefs": invocation.evidence_refs,
            },
            "authority": {
                "maxAttempts": 1,
                "retryFailed": false,
                "retryExpired": false,
                "computerUseAllowed": false,
                "bypassPermissions": false,
            },
        });
        let evidence_text = serde_json::to_string_pretty(&safe_evidence).expect("live evidence");
        for forbidden in [
            workspace.path().to_str().unwrap(),
            "credential",
            "leaseToken",
            "currentObservation",
            "objective",
        ] {
            assert!(
                !evidence_text.contains(forbidden),
                "live composition evidence leaked {forbidden}: {evidence_text}"
            );
        }
        fs::create_dir_all(&gate.evidence_dir).expect("live evidence directory");
        fs::write(
            gate.evidence_dir
                .join(format!("composed-{}.json", profile_name(profile))),
            evidence_text,
        )
        .expect("write live composition evidence");
    }

    // A successful run does not revoke the caller-owned upstream lease; the
    // adapter only revokes it on unproved termination. The disposable
    // checkout, however, must always be gone before this lane returns.
    assert!(
        fs::read_dir(isolate_parent.path())
            .unwrap()
            .next()
            .is_none(),
        "isolated checkout must be cleaned up after managed execution"
    );

    let stop_report = public_server.stop_and_wait().await;
    assert!(
        stop_report.is_clean(),
        "public MCP server shutdown: {:?}",
        stop_report.errors
    );

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
async fn economy_and_high_assurance_host_cu_then_dependent_grok() {
    run_profile(AdaptiveProfile::Economy, true, None).await;
    run_profile(AdaptiveProfile::HighAssurance, false, None).await;
}

/// Explicit live-provider composition. The deterministic host Computer Use
/// step still exercises the same adaptive/restart path; only the dependent
/// isolated-review executor is switched to the authorized real Grok Build
/// CLI. This remains ignored so no provider call can occur accidentally.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit live Grok Build authorization"]
#[allow(clippy::await_holding_lock)]
async fn live_composed_host_cu_then_managed_grok_runs_both_profiles() {
    let gate = LiveGate::from_env();
    run_profile(AdaptiveProfile::Economy, true, Some(&gate)).await;
    run_profile(AdaptiveProfile::HighAssurance, false, Some(&gate)).await;
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
mkdir -p "$GROK_HOME/sessions/workspace/$session_id"
printf 'after\n' > DOGFOOD.txt
printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded managed dogfood\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
printf '{"text":"bounded managed dogfood\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
"#;
