//! Targeted repairs for workload reliability v2 draft #585.
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use chrono::Utc;
use grokptah_agent_bridge::grok_build::{
    CredentialLeaseHandle, CredentialLeaseResolver, GrokBuildAdapterError, GrokBuildGitIdentity,
};
use grokptah_agent_bridge::managed_provider::ManagedProviderEvidence;
use grokptah_agent_bridge::orchestration::{
    canonical_workspace, hash_payload, safe_id_filename, ManagedExecutionBudgetProfile,
    ManagedExecutionIntent, ManagedGrokCliPermissionMode, ManagedGrokExecutorConfig,
    ManagedGrokInvocation, ManagedIntentState, MessageKind, OrchErrorCode, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, RunExecutionMode, WorkPolicy, WorkState,
    WorkspaceAllowlist, MANAGED_EXECUTION_SCHEMA_VERSION, MANAGED_GROK_INVOCATION_SCHEMA_VERSION,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, HostConfig, HostRuntime, SessionKind, PRUNE_REMOVE_FAULT,
};
use serde_json::json;
use uuid::Uuid;

const TOKEN: &str = "wr-v2-585-repair";
const MAX_RETAINED_MESSAGES: usize = 500;

fn idempotency_receipt_status(ledger: &Path, request_id: &str) -> String {
    for entry in walkdir::WalkDir::new(ledger.join("idempotency"))
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if value["requestId"] == request_id {
            return value["status"].as_str().unwrap().to_string();
        }
    }
    panic!("idempotency receipt {request_id} not found under {ledger:?}");
}

fn write_json(path: impl AsRef<std::path::Path>, value: &serde_json::Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

struct RevokeOrderResolver {
    receipt_status_at_revoke: Arc<Mutex<Option<String>>>,
    receipt_reader: Arc<dyn Fn() -> String + Send + Sync>,
    revoke_count: Arc<AtomicU32>,
}

impl CredentialLeaseResolver for RevokeOrderResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let _ = lease_id;
        Err(GrokBuildAdapterError::CredentialLease)
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        let _ = lease_id;
        self.revoke_count.fetch_add(1, Ordering::SeqCst);
        *self.receipt_status_at_revoke.lock().unwrap() = Some((self.receipt_reader)());
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

async fn managed_cancel_fixture() -> (
    tempfile::TempDir,
    HostRuntime,
    Arc<OrchestrationService>,
    OrchStore,
    grokptah_agent_bridge::orchestration::AuthContext,
    Uuid,
    PathBuf,
    String,
    Arc<AtomicU32>,
    Arc<Mutex<Option<String>>>,
) {
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let root = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let workspace = canonical_workspace(&root.path().join("workspace")).unwrap();
    let runtime = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    runtime.start().unwrap();
    let session = runtime.session_new_kind(SessionKind::Build).unwrap().id;
    runtime.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    let orch = OrchestrationService::new(
        runtime.clone(),
        runtime.event_bus(),
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
    let work_id = created["work"]["workId"].as_str().unwrap().to_string();
    let item = store.load_work_item(&work_id).unwrap().unwrap();
    let lease_id = "managed-cancel-lease";
    let revoke_count = Arc::new(AtomicU32::new(0));
    let receipt_status_at_revoke = Arc::new(Mutex::new(None));
    let ledger_root = store.root().to_path_buf();
    let receipt_reader =
        Arc::new(move || idempotency_receipt_status(&ledger_root, "cancel-managed"))
            as Arc<dyn Fn() -> String + Send + Sync>;
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
            revoke_count: revoke_count.clone(),
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
    (
        root,
        runtime,
        orch,
        store,
        auth,
        session,
        workspace,
        work_id,
        revoke_count,
        receipt_status_at_revoke,
    )
}

#[tokio::test]
async fn authenticated_send_survives_post_delivery_prune_failure() {
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let root = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let workspace = canonical_workspace(&root.path().join("workspace")).unwrap();
    let runtime = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    runtime.start().unwrap();
    let session = runtime.session_new_kind(SessionKind::Build).unwrap().id;
    runtime.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    std::fs::create_dir_all(store.root().join("messages")).unwrap();
    for index in 1..=MAX_RETAINED_MESSAGES {
        let mut message = grokptah_agent_bridge::orchestration::WorkMessage::new(
            MessageKind::Status,
            "seed",
            None,
            None,
            session,
            workspace.display().to_string(),
            None,
            "retention seed",
            None,
            Utc::now(),
        )
        .unwrap();
        message.message_id = format!("retained-seed-{index:04}");
        message.seq = index as u64;
        let path = store.root().join(format!(
            "messages/{}.json",
            safe_id_filename(&message.message_id).unwrap()
        ));
        write_json(path, &json!(message));
    }
    std::fs::write(store.root().join("messages/seq"), "500").unwrap();
    let orch = OrchestrationService::new(
        runtime.clone(),
        runtime.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    PRUNE_REMOVE_FAULT.store(true, Ordering::SeqCst);
    let delivered = orch
        .send_message(
            &auth,
            "send-overflow",
            session,
            &workspace,
            MessageKind::Status,
            None,
            None,
            None,
            "overflow after retention".into(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    PRUNE_REMOVE_FAULT.store(false, Ordering::SeqCst);
    let message_id = delivered["message"]["messageId"].as_str().unwrap();
    let scope = grokptah_agent_bridge::orchestration::IdempotencyScope::new(
        &auth.owner_id,
        session,
        &workspace,
    )
    .unwrap();
    let receipt = store
        .load_idempotency(&scope, "send-overflow")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, "complete");
    assert!(store.ledger_admission_closed().is_none());
    assert!(store.load_message(message_id).unwrap().is_some());
    let replay = orch
        .send_message(
            &auth,
            "send-overflow",
            session,
            &workspace,
            MessageKind::Status,
            None,
            None,
            None,
            "overflow after retention".into(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(replay, delivered);
    let ledger_root = store.root().to_path_buf();
    drop(orch);
    drop(store);
    assert!(runtime.shutdown().await.is_clean());
    let message_path = ledger_root.join(format!(
        "messages/{}.json",
        safe_id_filename(message_id).unwrap()
    ));
    assert!(message_path.is_file());
    assert_eq!(
        std::fs::read_dir(ledger_root.join("messages"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count(),
        MAX_RETAINED_MESSAGES + 1
    );
}

#[tokio::test]
async fn cancel_work_revokes_managed_capability_before_receipt_settles() {
    let (
        _root,
        _runtime,
        orch,
        store,
        auth,
        session,
        workspace,
        work_id,
        _revoke_count,
        receipt_status_at_revoke,
    ) = managed_cancel_fixture().await;
    orch.cancel_work(
        &auth,
        "cancel-managed",
        session,
        &workspace,
        &work_id,
        "operator cancelled".into(),
        Some(store.load_work_item(&work_id).unwrap().unwrap().revision),
    )
    .await
    .unwrap();
    assert_eq!(
        receipt_status_at_revoke.lock().unwrap().as_deref(),
        Some("pending"),
        "managed capability must be revoked before the cancel receipt settles"
    );
    let scope = grokptah_agent_bridge::orchestration::IdempotencyScope::new(
        &auth.owner_id,
        session,
        &workspace,
    )
    .unwrap();
    assert_eq!(
        store
            .load_idempotency(&scope, "cancel-managed")
            .unwrap()
            .unwrap()
            .status,
        "complete"
    );
}

#[tokio::test]
async fn stale_cancel_does_not_revoke_or_mutate_work() {
    let (
        _root,
        _runtime,
        orch,
        store,
        auth,
        session,
        workspace,
        work_id,
        revoke_count,
        _receipt_status,
    ) = managed_cancel_fixture().await;
    let revision = store.load_work_item(&work_id).unwrap().unwrap().revision;
    let error = orch
        .cancel_work(
            &auth,
            "stale-cancel",
            session,
            &workspace,
            &work_id,
            "stale".into(),
            Some(revision + 9),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, OrchErrorCode::StaleVersion);
    assert_eq!(revoke_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().state,
        WorkState::Queued
    );
}

#[tokio::test]
async fn concurrent_cancel_revision_fence_blocks_stale_side_effects() {
    let (
        root,
        _runtime,
        orch,
        store,
        auth,
        session,
        workspace,
        work_id,
        revoke_count,
        _receipt_status,
    ) = managed_cancel_fixture().await;
    let revision = store.load_work_item(&work_id).unwrap().unwrap().revision;
    OrchStore::set_work_request_test_gate(Some(Arc::new(Barrier::new(2))));
    let orch_a = orch.clone();
    let auth_a = auth.clone();
    let workspace_a = workspace.clone();
    let work_id_a = work_id.clone();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            OrchStore::arrive_at_work_request_test_gate();
            orch_a
                .cancel_work(
                    &auth_a,
                    "stale-race",
                    session,
                    &workspace_a,
                    &work_id_a,
                    "stale race".into(),
                    Some(revision),
                )
                .await
        })
    });
    OrchStore::arrive_at_work_request_test_gate();
    store
        .reprioritize_work(&work_id, 99, "bump revision", "operator", None, Utc::now())
        .unwrap();
    let result = worker.join().unwrap();
    OrchStore::set_work_request_test_gate(None);
    assert_eq!(result.unwrap_err().code, OrchErrorCode::StaleVersion);
    assert_eq!(revoke_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().revision,
        revision + 1
    );
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().state,
        WorkState::Queued
    );
    let _ = root;
}

struct FailRevokeResolver {
    revoke_count: Arc<AtomicU32>,
}

impl CredentialLeaseResolver for FailRevokeResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let _ = lease_id;
        Err(GrokBuildAdapterError::CredentialLease)
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        let _ = lease_id;
        self.revoke_count.fetch_add(1, Ordering::SeqCst);
        Err(GrokBuildAdapterError::CredentialLease)
    }

    fn provider_evidence(&self, lease_id: &str) -> Option<ManagedProviderEvidence> {
        let _ = lease_id;
        Some(ManagedProviderEvidence {
            mechanism: "test-revoke-fail".into(),
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

#[tokio::test]
async fn cancel_revoke_conflict_leaves_work_active_and_retryable_receipt() {
    let (
        _root,
        _runtime,
        orch,
        store,
        auth,
        session,
        workspace,
        work_id,
        revoke_count,
        _receipt_status,
    ) = managed_cancel_fixture().await;
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable: PathBuf::from("/bin/true"),
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: workspace.clone(),
            isolate_parent: _root.path().join("isolate"),
            repository_id: "repo-wr-v2-repair".into(),
            base_ref: "refs/heads/main".into(),
            identity: GrokBuildGitIdentity {
                repository_id: "repo-wr-v2-repair".into(),
                git_ref: "refs/heads/main".into(),
                base_sha: "0000000000000000000000000000000000000001".into(),
                head_sha: "0000000000000000000000000000000000000001".into(),
            },
            credential_lease_id: "managed-cancel-lease".into(),
        },
        Arc::new(FailRevokeResolver {
            revoke_count: revoke_count.clone(),
        }),
    )
    .unwrap();
    let revision = store.load_work_item(&work_id).unwrap().unwrap().revision;
    let error = orch
        .cancel_work(
            &auth,
            "cancel-revoke-conflict",
            session,
            &workspace,
            &work_id,
            "revoke refused".into(),
            Some(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, OrchErrorCode::Conflict);
    assert_eq!(revoke_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load_work_item(&work_id).unwrap().unwrap().state,
        WorkState::Queued
    );
    let scope = grokptah_agent_bridge::orchestration::IdempotencyScope::new(
        &auth.owner_id,
        session,
        &workspace,
    )
    .unwrap();
    let receipt = store
        .load_idempotency(&scope, "cancel-revoke-conflict")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, "failed");
    assert_eq!(
        receipt.error.as_ref().map(|e| e.code.clone()),
        Some(OrchErrorCode::Conflict)
    );
    let retry = orch
        .cancel_work(
            &auth,
            "cancel-revoke-conflict",
            session,
            &workspace,
            &work_id,
            "revoke refused".into(),
            Some(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(retry.code, OrchErrorCode::Conflict);
    assert_eq!(
        store
            .load_idempotency(&scope, "cancel-revoke-conflict")
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
}
