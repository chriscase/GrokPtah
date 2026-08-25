//! Closed, secret-free authority model shared by hosted and desktop control.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::authz::canonical_workspace;
use super::types::{OrchError, OrchErrorCode, CONTROL_TOOLS};

pub const AUTHORITY_SCHEMA_VERSION: u32 = 1;
pub const AUTHORITY_CAPABILITY_SCHEMA: &str = "grokptah.authority-capabilities.v1";

const COMMON_HOST_CAPABILITIES: &[&str] = &[
    "durable_sessions",
    "durable_runs",
    "event_replay",
    "persistent_agents",
    "durable_work",
    "routines",
    "native_execution",
];

const DESKTOP_LOCAL_HOST_CAPABILITIES: &[&str] = &[
    "desktop_keychain",
    "desktop_pty",
    "desktop_local_approval",
    "semantic_computer_use_foreground",
];

/// Host shape captured when an authenticated control-plane attempt begins.
///
/// This is deliberately separate from the caller's authority role. A remote
/// bearer connected to a desktop host can learn that the *host* supports a
/// local feature without inheriting permission to invoke that feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostKind {
    DesktopLocal,
    StandaloneService,
    Unknown,
}

impl RuntimeHostKind {
    fn wire(self) -> &'static str {
        match self {
            Self::DesktopLocal => "desktop_local",
            Self::StandaloneService => "standalone_service",
            Self::Unknown => "unknown",
        }
    }
}

/// Immutable host facts bound into every per-credential capability document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostCapabilityProfile {
    asserted_by: CapabilityAssertion,
    capabilities: Vec<String>,
}

impl HostCapabilityProfile {
    pub(crate) fn for_runtime(kind: RuntimeHostKind, runtime_home: &Path) -> Self {
        let mut identity = Vec::new();
        identity.extend_from_slice(b"grokptah.host-instance.v1\0");
        identity.extend_from_slice(kind.wire().as_bytes());
        identity.push(0);
        identity.extend_from_slice(runtime_home.as_os_str().as_encoded_bytes());

        let mut capabilities = COMMON_HOST_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        if kind == RuntimeHostKind::DesktopLocal {
            capabilities.extend(
                DESKTOP_LOCAL_HOST_CAPABILITIES
                    .iter()
                    .map(|value| (*value).to_string()),
            );
        }
        capabilities.sort();
        capabilities.dedup();

        Self {
            asserted_by: CapabilityAssertion {
                host_instance_id: opaque_id(&identity),
                host_kind: kind.wire().to_string(),
                host_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityRole {
    LocalOperator,
    RemoteOperator,
    RemoteCoordinator,
    Observer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityOperation {
    CapabilitiesRead,
    SessionsRead,
    SessionsCreate,
    AgentsRead,
    AgentsResume,
    CapacityRead,
    ReadinessRead,
    RunsRead,
    RunsEvents,
    RunsSubmit,
    RunsSteer,
    RunsCancel,
    RunsReview,
    RunsRetry,
    RunsApprove,
    RunsPromote,
    RunsDiscard,
    QueueControl,
    ComputerRead,
    WorkRead,
    WorkCreate,
    WorkAssign,
    WorkClaim,
    WorkLease,
    WorkProgress,
    WorkComplete,
    WorkCancel,
    WorkRetry,
    WorkApprove,
    WorkAdmin,
    WorkMessages,
    ManagerRead,
    ManagerCreate,
    ManagerControl,
    RoutinesRead,
    RoutinesCreate,
    RoutinesControl,
    RoutinesFire,
    WorkersRead,
    WorkersControl,
    ManagedRead,
    ManagedConfigure,
    ManagedAuthorize,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceGrant {
    workspace_id: String,
    canonical_root: PathBuf,
}

impl std::fmt::Debug for WorkspaceGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceGrant")
            .field("workspace_id", &self.workspace_id)
            .field("canonical_root", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityStamp {
    pub schema_version: u32,
    pub principal_id: String,
    pub credential_id: String,
    pub owner_principal_id: String,
    pub role: AuthorityRole,
    pub grant_id: String,
    pub grant_revision: u64,
    pub capability_document_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityPrincipal {
    pub principal_id: String,
    pub credential_id: String,
    pub owner_principal_id: String,
    pub role: AuthorityRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityScopes {
    pub workspace_ids: Vec<String>,
    pub agent_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityAssertion {
    pub host_instance_id: String,
    pub host_kind: String,
    pub host_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityCapabilityDocument {
    pub schema: String,
    pub schema_version: u32,
    pub document_id: String,
    pub document_hash: String,
    pub asserted_by: CapabilityAssertion,
    pub principal: CapabilityPrincipal,
    pub scopes: CapabilityScopes,
    pub host_capabilities: Vec<String>,
    pub operations: Vec<String>,
    pub tools: Vec<String>,
    pub hard_denials: Vec<String>,
    pub config_revision: u64,
}

#[derive(Clone)]
pub(crate) struct EffectiveAuthority {
    pub(crate) stamp: AuthorityStamp,
    pub(crate) operations: BTreeSet<AuthorityOperation>,
    workspaces: Vec<WorkspaceGrant>,
    agent_ids: BTreeSet<String>,
    pub(crate) capability_document: AuthorityCapabilityDocument,
}

impl std::fmt::Debug for EffectiveAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectiveAuthority")
            .field("stamp", &self.stamp)
            .field("operations", &self.operations)
            .field(
                "workspace_ids",
                &self
                    .workspaces
                    .iter()
                    .map(|grant| grant.workspace_id.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("agent_ids", &self.agent_ids)
            .field("capability_document", &self.capability_document)
            .finish()
    }
}

impl EffectiveAuthority {
    pub(crate) fn remote_default(
        credential_id: &str,
        owner_principal_id: &str,
        roots: &[PathBuf],
        role: AuthorityRole,
        computer_read: bool,
    ) -> Result<Self, OrchError> {
        if role == AuthorityRole::LocalOperator {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "a bearer credential cannot request local_operator authority",
            ));
        }
        let workspaces = workspace_grants(roots)?;
        let mut operations = role_ceiling(role);
        if computer_read {
            operations.insert(AuthorityOperation::ComputerRead);
        }
        Self::new(
            credential_id,
            owner_principal_id,
            role,
            workspaces,
            operations,
            "standalone_service",
        )
    }

    pub(crate) fn trusted_local_operator(
        owner_principal_id: &str,
        roots: &[PathBuf],
    ) -> Result<Self, OrchError> {
        Self::new(
            "trusted-local-adapter",
            owner_principal_id,
            AuthorityRole::LocalOperator,
            workspace_grants(roots)?,
            role_ceiling(AuthorityRole::LocalOperator),
            "desktop_local",
        )
    }

    /// Narrow a remote authority document to one durable Agent identity.
    /// Recompute the document hash so the published stamp and its audit
    /// evidence cannot describe a broader scope than enforcement uses.
    pub(crate) fn with_agent_scope(mut self, agent_id: &str) -> Result<Self, OrchError> {
        validate_id(agent_id, "agent scope")?;
        self.agent_ids.insert(agent_id.to_string());
        self.capability_document.scopes.agent_ids = self.agent_ids.iter().cloned().collect();
        self.capability_document.document_hash = hash_document(&self.capability_document)?;
        self.stamp.capability_document_hash = self.capability_document.document_hash.clone();
        Ok(self)
    }

    /// Bind the attempt-time host profile without widening caller authority.
    /// Re-hashing makes the transport session fail closed if the host shape
    /// changes across a reconnect or credential rotation.
    pub(crate) fn with_host_profile(
        mut self,
        profile: &HostCapabilityProfile,
    ) -> Result<Self, OrchError> {
        self.capability_document.asserted_by = profile.asserted_by.clone();
        self.capability_document.host_capabilities = profile.capabilities.clone();
        self.capability_document.document_hash = hash_document(&self.capability_document)?;
        self.stamp.capability_document_hash = self.capability_document.document_hash.clone();
        Ok(self)
    }

    fn new(
        credential_id: &str,
        owner_principal_id: &str,
        role: AuthorityRole,
        workspaces: Vec<WorkspaceGrant>,
        operations: BTreeSet<AuthorityOperation>,
        host_kind: &str,
    ) -> Result<Self, OrchError> {
        validate_id(credential_id, "credential id")?;
        validate_id(owner_principal_id, "owner principal id")?;
        let principal_id = format!("{owner_principal_id}:{credential_id}");
        let workspace_ids = workspaces
            .iter()
            .map(|grant| grant.workspace_id.clone())
            .collect::<Vec<_>>();
        let tools = CONTROL_TOOLS
            .iter()
            .filter(|tool| {
                required_operation(tool).is_some_and(|operation| operations.contains(&operation))
            })
            .map(|tool| (*tool).to_string())
            .collect::<Vec<_>>();
        let grant_id = format!("{credential_id}-{}-v1", role_wire(role));
        let document_id = format!("authority-{grant_id}");
        let asserted_by = CapabilityAssertion {
            host_instance_id: opaque_id(owner_principal_id.as_bytes()),
            host_kind: host_kind.to_string(),
            host_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let principal = CapabilityPrincipal {
            principal_id: principal_id.clone(),
            credential_id: credential_id.to_string(),
            owner_principal_id: owner_principal_id.to_string(),
            role,
        };
        let scopes = CapabilityScopes {
            workspace_ids,
            agent_ids: Vec::new(),
        };
        let host_capabilities = COMMON_HOST_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        let hard_denials = match role {
            AuthorityRole::LocalOperator => Vec::new(),
            AuthorityRole::RemoteOperator => vec!["computer_use".to_string()],
            AuthorityRole::RemoteCoordinator | AuthorityRole::Observer => vec![
                "approval".to_string(),
                "promotion".to_string(),
                "computer_use".to_string(),
            ],
        };
        let mut document = AuthorityCapabilityDocument {
            schema: AUTHORITY_CAPABILITY_SCHEMA.to_string(),
            schema_version: AUTHORITY_SCHEMA_VERSION,
            document_id,
            document_hash: String::new(),
            asserted_by,
            principal,
            scopes,
            host_capabilities,
            operations: operations
                .iter()
                .copied()
                .map(operation_wire)
                .map(str::to_string)
                .collect(),
            tools,
            hard_denials,
            config_revision: 1,
        };
        document.document_hash = hash_document(&document)?;
        let stamp = AuthorityStamp {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            principal_id,
            credential_id: credential_id.to_string(),
            owner_principal_id: owner_principal_id.to_string(),
            role,
            grant_id,
            grant_revision: 1,
            capability_document_hash: document.document_hash.clone(),
        };
        Ok(Self {
            stamp,
            operations,
            workspaces,
            agent_ids: BTreeSet::new(),
            capability_document: document,
        })
    }

    pub(crate) fn require_operation(&self, operation: AuthorityOperation) -> Result<(), OrchError> {
        if self.operations.contains(&operation) {
            Ok(())
        } else {
            Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                format!(
                    "authority does not grant operation {}",
                    operation_wire(operation)
                ),
            ))
        }
    }

    pub(crate) fn require_workspace(
        &self,
        operation: AuthorityOperation,
        claimed: &Path,
    ) -> Result<PathBuf, OrchError> {
        self.require_operation(operation)?;
        let canonical = canonical_workspace(claimed)?;
        if self
            .workspaces
            .iter()
            .any(|grant| grant.canonical_root == canonical)
        {
            Ok(canonical)
        } else {
            Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "workspace is outside the credential grant",
            ))
        }
    }
}

pub fn required_operation(tool: &str) -> Option<AuthorityOperation> {
    use AuthorityOperation::*;
    Some(match tool {
        "ptah_get_authority_capabilities" => CapabilitiesRead,
        "ptah_list_sessions" => SessionsRead,
        "ptah_create_session" => SessionsCreate,
        "ptah_list_persistent_agents" | "ptah_get_persistent_agent" => AgentsRead,
        "ptah_resume_persistent_agent" => AgentsResume,
        "ptah_get_capacity" => CapacityRead,
        "ptah_get_native_coding_readiness" => ReadinessRead,
        "ptah_list_runs"
        | "ptah_get_run"
        | "ptah_get_progress"
        | "ptah_get_changes"
        | "ptah_get_test_results"
        | "ptah_get_handoff" => RunsRead,
        "ptah_get_events" => RunsEvents,
        "ptah_submit_task" => RunsSubmit,
        "ptah_steer" | "ptah_steer_queued" => RunsSteer,
        "ptah_cancel" => RunsCancel,
        "ptah_review_run" => RunsReview,
        "ptah_retry_run" => RunsRetry,
        "ptah_approve_run" => RunsApprove,
        "ptah_promote_run" => RunsPromote,
        "ptah_discard_run" => RunsDiscard,
        "ptah_get_queue" | "ptah_queue_prompt" | "ptah_edit_queue" | "ptah_remove_queue"
        | "ptah_reorder_queue" | "ptah_clear_queue" | "ptah_run_next" => QueueControl,
        "ptah_list_computer_runs"
        | "ptah_get_computer_run"
        | "ptah_get_computer_run_events"
        | "ptah_get_computer_capacity" => ComputerRead,
        "ptah_list_work" | "ptah_get_work" | "ptah_list_work_decisions" => WorkRead,
        "ptah_create_work" | "ptah_create_enterprise_review" => WorkCreate,
        "ptah_assign_work" => WorkAssign,
        "ptah_claim_work" => WorkClaim,
        "ptah_renew_work" | "ptah_release_work" => WorkLease,
        "ptah_link_work_run" | "ptah_report_work_progress" => WorkProgress,
        "ptah_complete_work" | "ptah_fail_work" => WorkComplete,
        "ptah_cancel_work" => WorkCancel,
        "ptah_retry_work" => WorkRetry,
        "ptah_approve_work" => WorkApprove,
        "ptah_reassign_work"
        | "ptah_reprioritize_work"
        | "ptah_block_work"
        | "ptah_request_review" => WorkAdmin,
        "ptah_send_message" | "ptah_ack_message" | "ptah_list_inbox" | "ptah_list_outbox" => {
            WorkMessages
        }
        "ptah_list_manager_plans" | "ptah_get_manager_plan" => ManagerRead,
        "ptah_create_manager_plan" => ManagerCreate,
        "ptah_advance_manager_plan" | "ptah_tick_manager_plan" | "ptah_replan_manager_plan" => {
            ManagerControl
        }
        "ptah_list_routines" | "ptah_get_routine" | "ptah_list_activations" => RoutinesRead,
        "ptah_create_routine" => RoutinesCreate,
        "ptah_pause_routine" | "ptah_enable_routine" | "ptah_disable_routine" => RoutinesControl,
        "ptah_fire_routine" => RoutinesFire,
        "ptah_list_workers" | "ptah_get_worker" => WorkersRead,
        "ptah_heartbeat_worker" | "ptah_offer_work" | "ptah_accept_work" | "ptah_decline_work" => {
            WorkersControl
        }
        "ptah_get_managed_execution" | "ptah_list_execution_intents" => ManagedRead,
        "ptah_set_managed_execution" => ManagedConfigure,
        "ptah_authorize_work_execution" | "ptah_resolve_work_input" => ManagedAuthorize,
        _ => return None,
    })
}

pub fn role_ceiling(role: AuthorityRole) -> BTreeSet<AuthorityOperation> {
    use AuthorityOperation::*;
    match role {
        AuthorityRole::LocalOperator => CONTROL_TOOLS
            .iter()
            .filter_map(|tool| required_operation(tool))
            .collect(),
        AuthorityRole::RemoteOperator => [
            CapabilitiesRead,
            SessionsRead,
            SessionsCreate,
            AgentsRead,
            AgentsResume,
            CapacityRead,
            ReadinessRead,
            RunsRead,
            RunsEvents,
            RunsSubmit,
            RunsSteer,
            RunsCancel,
            RunsReview,
            RunsRetry,
            RunsApprove,
            RunsPromote,
            RunsDiscard,
            QueueControl,
            WorkRead,
            WorkCreate,
            WorkAssign,
            WorkClaim,
            WorkLease,
            WorkProgress,
            WorkComplete,
            WorkCancel,
            WorkRetry,
            WorkApprove,
            WorkAdmin,
            WorkMessages,
            ManagerRead,
            ManagerCreate,
            ManagerControl,
            RoutinesRead,
            RoutinesCreate,
            RoutinesControl,
            RoutinesFire,
            WorkersRead,
            WorkersControl,
            ManagedRead,
            ManagedConfigure,
            ManagedAuthorize,
        ]
        .into_iter()
        .collect(),
        AuthorityRole::RemoteCoordinator => [
            CapabilitiesRead,
            SessionsRead,
            SessionsCreate,
            AgentsRead,
            AgentsResume,
            CapacityRead,
            ReadinessRead,
            RunsSubmit,
            RunsRead,
            RunsEvents,
            RunsSteer,
            RunsCancel,
            RunsReview,
            RunsRetry,
            QueueControl,
            WorkRead,
            WorkCreate,
            WorkAssign,
            WorkClaim,
            WorkLease,
            WorkProgress,
            WorkComplete,
            WorkRetry,
            WorkCancel,
            WorkAdmin,
            WorkMessages,
            ManagerRead,
            ManagerCreate,
            ManagerControl,
            WorkersRead,
            WorkersControl,
            ManagedRead,
            RoutinesRead,
            RoutinesCreate,
            RoutinesControl,
            RoutinesFire,
        ]
        .into_iter()
        .collect(),
        AuthorityRole::Observer => [
            CapabilitiesRead,
            SessionsRead,
            AgentsRead,
            RunsRead,
            RunsEvents,
            WorkRead,
            ManagedRead,
            RoutinesRead,
        ]
        .into_iter()
        .collect(),
    }
}

pub fn validate_tool_registry() -> Result<(), OrchError> {
    for tool in CONTROL_TOOLS {
        if required_operation(tool).is_none() {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                format!("control tool {tool} has no authority operation"),
            ));
        }
    }
    Ok(())
}

fn workspace_grants(roots: &[PathBuf]) -> Result<Vec<WorkspaceGrant>, OrchError> {
    if roots.len() > 64 {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "authority grant exceeds 64 workspaces",
        ));
    }
    roots
        .iter()
        .map(|root| {
            let canonical_root = canonical_workspace(root)?;
            let workspace_id = format!(
                "workspace-{}",
                opaque_id(canonical_root.as_os_str().as_encoded_bytes())
            );
            Ok(WorkspaceGrant {
                workspace_id,
                canonical_root,
            })
        })
        .collect()
}

fn hash_document(document: &AuthorityCapabilityDocument) -> Result<String, OrchError> {
    let mut value = serde_json::to_value(document)
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
    value["documentHash"] = serde_json::Value::String(String::new());
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
    Ok(opaque_id(&encoded))
}

fn opaque_id(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_id(value: &str, label: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{label} is invalid"),
        ));
    }
    Ok(())
}

fn role_wire(role: AuthorityRole) -> &'static str {
    match role {
        AuthorityRole::LocalOperator => "local-operator",
        AuthorityRole::RemoteOperator => "remote-operator",
        AuthorityRole::RemoteCoordinator => "remote-coordinator",
        AuthorityRole::Observer => "observer",
    }
}

fn operation_wire(operation: AuthorityOperation) -> &'static str {
    use AuthorityOperation::*;
    match operation {
        CapabilitiesRead => "capabilities.read",
        SessionsRead => "sessions.read",
        SessionsCreate => "sessions.create",
        AgentsRead => "agents.read",
        AgentsResume => "agents.resume",
        CapacityRead => "capacity.read",
        ReadinessRead => "readiness.read",
        RunsRead => "runs.read",
        RunsEvents => "runs.events",
        RunsSubmit => "runs.submit",
        RunsSteer => "runs.steer",
        RunsCancel => "runs.cancel",
        RunsReview => "runs.review",
        RunsRetry => "runs.retry",
        RunsApprove => "runs.approve",
        RunsPromote => "runs.promote",
        RunsDiscard => "runs.discard",
        QueueControl => "queue.control",
        ComputerRead => "computer.read",
        WorkRead => "work.read",
        WorkCreate => "work.create",
        WorkAssign => "work.assign",
        WorkClaim => "work.claim",
        WorkLease => "work.lease",
        WorkProgress => "work.progress",
        WorkComplete => "work.complete",
        WorkCancel => "work.cancel",
        WorkRetry => "work.retry",
        WorkApprove => "work.approve",
        WorkAdmin => "work.admin",
        WorkMessages => "work.messages",
        ManagerRead => "manager.read",
        ManagerCreate => "manager.create",
        ManagerControl => "manager.control",
        RoutinesRead => "routines.read",
        RoutinesCreate => "routines.create",
        RoutinesControl => "routines.control",
        RoutinesFire => "routines.fire",
        WorkersRead => "workers.read",
        WorkersControl => "workers.control",
        ManagedRead => "managed.read",
        ManagedConfigure => "managed.configure",
        ManagedAuthorize => "managed.authorize",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn registry_is_total_and_remote_ceiling_excludes_operator_power() {
        validate_tool_registry().unwrap();
        let root = tempdir().unwrap();
        let authority = EffectiveAuthority::remote_default(
            "remote",
            "owner",
            &[root.path().to_path_buf()],
            AuthorityRole::RemoteCoordinator,
            false,
        )
        .unwrap();
        for denied in [
            "ptah_approve_work",
            "ptah_approve_run",
            "ptah_promote_run",
            "ptah_authorize_work_execution",
            "ptah_resolve_work_input",
            "ptah_set_managed_execution",
            "ptah_list_computer_runs",
        ] {
            assert!(!authority
                .capability_document
                .tools
                .iter()
                .any(|tool| tool == denied));
        }
        assert!(authority
            .capability_document
            .tools
            .iter()
            .any(|tool| tool == "ptah_submit_task"));
        assert_eq!(authority.capability_document.hard_denials.len(), 3);
    }

    #[test]
    fn bearer_cannot_mint_local_operator_authority() {
        let root = tempdir().unwrap();
        let remote = EffectiveAuthority::remote_default(
            "forged-local",
            "owner",
            &[root.path().to_path_buf()],
            AuthorityRole::LocalOperator,
            false,
        );
        assert!(remote.is_err());

        let trusted =
            EffectiveAuthority::trusted_local_operator("owner", &[root.path().to_path_buf()])
                .unwrap();
        assert_eq!(trusted.stamp.role, AuthorityRole::LocalOperator);
        assert_eq!(trusted.stamp.credential_id, "trusted-local-adapter");
    }

    #[test]
    fn host_profile_is_stable_role_separate_and_hash_bound() {
        let root = tempdir().unwrap();
        let authority = EffectiveAuthority::remote_default(
            "coordinator",
            "owner",
            &[root.path().to_path_buf()],
            AuthorityRole::RemoteCoordinator,
            false,
        )
        .unwrap();
        let operations = authority.operations.clone();
        let desktop_profile =
            HostCapabilityProfile::for_runtime(RuntimeHostKind::DesktopLocal, root.path());
        let desktop_again =
            HostCapabilityProfile::for_runtime(RuntimeHostKind::DesktopLocal, root.path());
        let service_profile =
            HostCapabilityProfile::for_runtime(RuntimeHostKind::StandaloneService, root.path());

        assert_eq!(desktop_profile, desktop_again);
        assert_ne!(
            desktop_profile.asserted_by.host_instance_id,
            service_profile.asserted_by.host_instance_id
        );

        let desktop = authority
            .clone()
            .with_host_profile(&desktop_profile)
            .unwrap();
        let service = authority.with_host_profile(&service_profile).unwrap();
        assert_eq!(desktop.operations, operations);
        assert_eq!(service.operations, operations);
        assert_eq!(
            desktop.capability_document.asserted_by.host_kind,
            "desktop_local"
        );
        assert_eq!(
            service.capability_document.asserted_by.host_kind,
            "standalone_service"
        );
        assert!(desktop
            .capability_document
            .host_capabilities
            .iter()
            .any(|value| value == "semantic_computer_use_foreground"));
        assert!(!service
            .capability_document
            .host_capabilities
            .iter()
            .any(|value| value == "semantic_computer_use_foreground"));
        assert_ne!(
            desktop.capability_document.document_hash,
            service.capability_document.document_hash
        );
        let encoded = serde_json::to_string(&desktop.capability_document).unwrap();
        assert!(!encoded.contains(root.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn remote_operator_ceiling_is_explicitly_complete_except_computer_read() {
        let expected = CONTROL_TOOLS
            .iter()
            .filter_map(|tool| required_operation(tool))
            .filter(|operation| *operation != AuthorityOperation::ComputerRead)
            .collect::<BTreeSet<_>>();
        assert_eq!(role_ceiling(AuthorityRole::RemoteOperator), expected);
    }

    #[test]
    fn observer_is_read_only_and_document_is_secret_free() {
        let root = tempdir().unwrap();
        let authority = EffectiveAuthority::remote_default(
            "observer",
            "owner",
            &[root.path().to_path_buf()],
            AuthorityRole::Observer,
            false,
        )
        .unwrap();
        assert!(authority
            .capability_document
            .tools
            .iter()
            .any(|tool| tool == "ptah_list_runs"));
        for denied in [
            AuthorityOperation::AgentsResume,
            AuthorityOperation::ManagerControl,
            AuthorityOperation::QueueControl,
            AuthorityOperation::RoutinesControl,
            AuthorityOperation::RunsCancel,
            AuthorityOperation::RunsSubmit,
            AuthorityOperation::WorkCreate,
            AuthorityOperation::WorkersControl,
        ] {
            assert!(!authority.operations.contains(&denied));
        }
        let encoded = serde_json::to_string(&authority.capability_document).unwrap();
        assert!(!encoded.contains(root.path().to_string_lossy().as_ref()));
        assert!(!encoded.to_ascii_lowercase().contains("bearer"));
        let debug = format!("{authority:?}");
        assert!(!debug.contains(root.path().to_string_lossy().as_ref()));
        assert!(authority
            .capability_document
            .operations
            .iter()
            .all(|operation| operation.contains('.')));
    }

    #[test]
    fn remote_operator_gets_protected_run_actions_but_never_computer_use() {
        let root = tempdir().unwrap();
        let authority = EffectiveAuthority::remote_default(
            "operator",
            "owner",
            &[root.path().to_path_buf()],
            AuthorityRole::RemoteOperator,
            false,
        )
        .unwrap();
        for allowed in ["ptah_approve_work", "ptah_approve_run", "ptah_promote_run"] {
            assert!(authority
                .capability_document
                .tools
                .iter()
                .any(|tool| tool == allowed));
        }
        for denied in [
            "ptah_list_computer_runs",
            "ptah_get_computer_run",
            "ptah_get_computer_run_events",
            "ptah_get_computer_capacity",
        ] {
            assert!(!authority
                .capability_document
                .tools
                .iter()
                .any(|tool| tool == denied));
        }
        assert_eq!(
            authority.capability_document.hard_denials,
            vec!["computer_use"]
        );
    }

    #[test]
    fn scoped_computer_read_grant_adds_only_read_capability() {
        let root = tempdir().unwrap();
        let authority = EffectiveAuthority::remote_default(
            "scoped",
            "owner",
            &[root.path().to_path_buf()],
            AuthorityRole::RemoteCoordinator,
            true,
        )
        .unwrap();
        assert!(authority
            .capability_document
            .tools
            .iter()
            .any(|tool| tool == "ptah_list_computer_runs"));
        for denied in [
            "ptah_approve_work",
            "ptah_promote_run",
            "ptah_authorize_work_execution",
        ] {
            assert!(!authority
                .capability_document
                .tools
                .iter()
                .any(|tool| tool == denied));
        }
    }
}
