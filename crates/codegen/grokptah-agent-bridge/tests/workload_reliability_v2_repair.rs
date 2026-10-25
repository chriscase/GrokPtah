//! Targeted repairs for workload reliability v2 draft #585.
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use grokptah_agent_bridge::grok_build::{
    CredentialLeaseHandle, CredentialLeaseResolver, GrokBuildAdapterError, GrokBuildGitIdentity,
};
use grokptah_agent_bridge::managed_provider::ManagedProviderEvidence;
use grokptah_agent_bridge::orchestration::{
    canonical_workspace, hash_payload, safe_id_filename,
    ManagedExecutionBudgetProfile, ManagedExecutionIntent, ManagedGrokCliPermissionMode,
    ManagedGrokInvocation, ManagedGrokExecutorConfig, ManagedIntentState, MessageKind,
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunExecutionMode,
    WorkMessage, WorkPolicy, WorkspaceAllowlist,
    MANAGED_EXECUTION_SCHEMA_VERSION, MANAGED_GROK_INVOCATION_SCHEMA_VERSION,
};
const MAX_RETAINED_MESSAGES: usize = 500;
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, DurableWriteFault, HostConfig, SessionKind,
    DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use uuid::Uuid;

const TOKEN: &str = "wr-v2-585-repair";
const MANAGER_REQUEST: &str = "original-manager-mutation";

fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn manager_child(root: &Path, operation: &str, phase: &str, stage: &str) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "manager_mutation_child", "--nocapture"])
        .env("MANAGER_MUTATION_ROOT", root)
        .env("MANAGER_MUTATION_OP", operation)
        .env("MANAGER_MUTATION_PHASE", phase)
        .env("MANAGER_MUTATION_STAGE", stage)
        .env_remove("GROKPTAH_CRASH_STAGE")
        .output()
        .unwrap()
}

fn manager_receipt(root: &Path) -> Value {
    walkdir::WalkDir::new(root.join("home/ledger/idempotency"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| read(entry.path()))
        .find(|value| value["requestId"] == MANAGER_REQUEST)
        .unwrap()
}

struct RevokeOrderResolver {
    receipt_status_at_revoke: Arc<Mutex<Option<String>>>,
    receipt_reader: Arc<dyn Fn() -> String + Send + Sync>,
}

impl CredentialLeaseResolver for RevokeOrderResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let _ = lease_id;
        Err(GrokBuildAdapterError::CredentialLease)
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        let _ = lease_id;
        *self.receipt_status_at_revoke.lock().unwrap() =
            Some((self.receipt_reader)());
        Ok(())
    }

    fn provider_evidence(&self, lease_id: &str) -> Option<ManagedProviderEvidence> {
        let _ = lease_id;
        Some(ManagedProviderEvidence {
            mechanism: "test-revoke-order".into(),
            model: "grok-build-0.1".into(),
            requests_reserved: 1,
            wire_attempts: 0,
            responses_completed: 0,
            denied_requests: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            usage_observed: false,
            accounting_complete: false,
            uncertain: false,
            revoked: false,
            authority_attempts: Vec::new(),
            ..Default::default()
        })
    }
}

fn minimal_grok_identity() -> grokptah_agent_sdk::GrokBuildGitIdentity {
    grokptah_agent_sdk::GrokBuildGitIdentity {
        repository_id: "repo-wr-v2-repair".into(),
        git_ref: "refs/heads/main".into(),
        base_sha: "0000000000000000000000000000000000000001".into(),
        head_sha: "0000000000000000000000000000000000000001".into(),
    }
}

#[test]
fn message_prune_remove_failures_surface_as_errors() {
    let store = OrchStore::open(tempfile::tempdir().unwrap().path()).unwrap();
    let session = Uuid::new_v4();
    let workspace = "/tmp/wr-v2-prune-fail-closed".to_string();
    for index in 1..=MAX_RETAINED_MESSAGES {
        let mut message = WorkMessage::new(
            MessageKind::Status,
            "seed",
            None,
            None,
            session,
            workspace.into(),
            None,
            "retention seed",
            None,
            Utc::now(),
        )
        .unwrap();
        message.message_id = format!("seed-{index:04}");
        message.seq = index as u64;
        let path = store
            .root()
            .join(format!("messages/{}.json", safe_id_filename(&message.message_id).unwrap()));
        write(path, &json!(message));
    }
    std::fs::write(store.root().join("messages/seq"), MAX_RETAINED_MESSAGES.to_string()).unwrap();

    *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
        needle: "messages/",
        skips_remaining: 0,
    });
    let mut overflow = WorkMessage::new(
        MessageKind::Status,
        "seed",
        None,
        None,
        session,
        workspace.into(),
        None,
        "overflow",
        None,
        Utc::now(),
    )
    .unwrap();
    overflow.message_id = "overflow-0001".into();
    let result = store.send_message(overflow);
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;

    assert!(
        result.is_err(),
        "prune must fail closed when retention removal fails: {result:?}"
    );
    let messages = std::fs::read_dir(store.root().join("messages"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    assert_eq!(
        messages,
        MAX_RETAINED_MESSAGES + 1,
        "fail-open prune must not leave stale messages behind"
    );
}

#[test]
fn manager_mutation_returns_success_when_only_post_commit_prune_fails() {
    let root = tempfile::tempdir().unwrap();
    write(root.path().join("options.json"), &json!({"seedMessages":500}));
    *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
        needle: "messages/",
        skips_remaining: 0,
    });
    let output = manager_child(root.path(), "tick-message", "replay", "");
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome = read(root.path().join("replay.json"));
    assert!(
        outcome.get("response").is_some(),
        "committed manager mutation must not surface as caller failure: {outcome}"
    );
    assert!(outcome.get("error").is_none(), "{outcome}");
    let receipt = manager_receipt(root.path());
    assert_eq!(receipt["status"], "complete");
    assert!(
        root.path()
            .join("home/ledger/manager-intents")
            .read_dir()
            .unwrap()
            .next()
            .is_none(),
        "intent cleanup must still complete"
    );
}

#[tokio::test]
async fn cancel_work_revokes_managed_capability_before_receipt_settles() {
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let root = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let workspace = canonical_workspace(&root.path().join("workspace")).unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap().id;
    host.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    let created = orch
        .create_work(
            &auth,
            "create-managed-cancel",
            session,
            &workspace,
            "test".into(),
            "managed cancel ordering".into(),
            0,
            None,
            None,
            Vec::new(),
            WorkPolicy::default(),
        )
        .await
        .unwrap();
    let work_id = created["work"]["workId"].as_str().unwrap();
    let item = store.load_work_item(work_id).unwrap().unwrap();
    let lease_id = "managed-cancel-lease";
    let receipt_status_at_revoke = Arc::new(Mutex::new(None));
    let receipt_reader = {
        let store = store.clone();
        let scope =
            grokptah_agent_bridge::orchestration::IdempotencyScope::new(&auth.owner_id, session, &workspace)
                .unwrap();
        Arc::new(move || {
            store
                .load_idempotency(&scope, "cancel-managed")
                .unwrap()
                .unwrap()
                .status
        }) as Arc<dyn Fn() -> String + Send + Sync>
    };
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable: PathBuf::from("/bin/true"),
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: workspace.clone(),
            isolate_parent: root.path().join("isolate"),
            repository_id: "repo-wr-v2-repair".into(),
            base_ref: "refs/heads/main".into(),
            identity: GrokBuildGitIdentity {
                repository_id: "repo-wr-v2-repair".into(),
                git_ref: "refs/heads/main".into(),
                base_sha: "0000000000000000000000000000000000000001".into(),
                head_sha: "0000000000000000000000000000000000000001".into(),
            },
            credential_lease_id: lease_id.into(),
        },
        Arc::new(RevokeOrderResolver {
            receipt_status_at_revoke: receipt_status_at_revoke.clone(),
            receipt_reader,
        }),
    )
    .unwrap();
    let now = Utc::now();
    let invocation = ManagedGrokInvocation {
        schema_version: MANAGED_GROK_INVOCATION_SCHEMA_VERSION,
        profile: ManagedExecutionBudgetProfile::Balanced,
        identity: minimal_grok_identity(),
        request_id: "dispatch-1".into(),
        dispatch_nonce: "nonce-1".into(),
        credential_alias_hash: hash_payload(&json!({"credentialLeaseAlias": lease_id})),
        prompt_hash: hash_payload(&json!({"prompt":"x"})),
        cli_permission_mode: ManagedGrokCliPermissionMode::HostMappedBypassPermissions,
        host_execution_approved: true,
        final_head_sha: None,
        final_ref: None,
        final_state: None,
        verdict: None,
        evidence_refs: Vec::new(),
        changed_paths: Vec::new(),
        diff_digest: None,
        source_fingerprint: None,
        credential_lease_id: Some(lease_id.into()),
        provider_evidence: Some(ManagedProviderEvidence {
            mechanism: "test-revoke-order".into(),
            model: "grok-build-0.1".into(),
            requests_reserved: 1,
            wire_attempts: 0,
            responses_completed: 0,
            denied_requests: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            usage_observed: false,
            accounting_complete: false,
            uncertain: false,
            revoked: false,
            authority_attempts: Vec::new(),
            ..Default::default()
        }),
    };
    store
        .save_managed_intent(&ManagedExecutionIntent {
            schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
            intent_id: "managed-cancel-intent".into(),
            agent_id: "operator".into(),
            agent_spec_revision: 1,
            work_id: item.work_id.clone(),
            work_revision: item.revision,
            attempt_id: None,
            run_id: None,
            session_id: item.session_id,
            workspace: item.workspace.clone(),
            source_routine_id: None,
            source_activation_id: None,
            model_selection_key: "grok".into(),
            bounds: RunBounds::default(),
            execution_mode: RunExecutionMode::Shared,
            input_hash: "hash".into(),
            grok: Some(invocation),
            state: ManagedIntentState::Admitted,
            permission_request_id: None,
            created_at: now,
            updated_at: now,
        })
        .unwrap();

    orch.cancel_work(
        &auth,
        "cancel-managed",
        session,
        &workspace,
        work_id,
        "operator cancelled".into(),
        Some(item.revision),
    )
    .await
    .unwrap();

    assert_eq!(
        receipt_status_at_revoke.lock().unwrap().as_deref(),
        Some("pending"),
        "managed capability must be revoked before the cancel receipt settles"
    );
    let receipt = store.load_idempotency(
        &grokptah_agent_bridge::orchestration::IdempotencyScope::new(
            &auth.owner_id,
            session,
            &workspace,
        )
        .unwrap(),
        "cancel-managed",
    )
    .unwrap()
    .unwrap();
    assert_eq!(receipt.status, "complete");
}
