//! Strict execution overlay for the authoritative persistent-Agent catalog.
//!
//! This module intentionally does not define promotable provider scenarios.
//! [`ProbeDefinition`] values are subordinate black-box exercises that point
//! at scenario IDs owned by `evals/persistent-agent-scenarios.v1.json`.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::scan_value_for_forbidden_data;
pub use grokptah_agent_bridge::CertificationBoundProfile as BoundProfile;
use serde::{Deserialize, Serialize};

use crate::LAB_MANIFEST_SCHEMA;

pub const AUTHORITATIVE_CATALOG_SCHEMA: &str = "grokptah.persistent_agent_scenarios.v1";
pub const AUTHORITATIVE_CATALOG_ID: &str = "persistent-agent-certification-v1";
pub const AUTHORITATIVE_CATALOG_VERSION: u32 = 1;
pub const AUTHORITATIVE_CATALOG_RELATIVE_PATH: &str = "../persistent-agent-scenarios.v1.json";

// Public MCP names from the merged managed-execution contract. The lab still
// treats its six native probes as future implementation work: discovering
// these tools makes an unimplemented probe indeterminate, never passing.
pub const FUTURE_SET_MANAGED_EXECUTION_TOOL: &str = "ptah_set_managed_execution";
pub const FUTURE_GET_MANAGED_EXECUTION_TOOL: &str = "ptah_get_managed_execution";
pub const FUTURE_AUTHORIZE_WORK_EXECUTION_TOOL: &str = "ptah_authorize_work_execution";
pub const FUTURE_RESOLVE_WORK_INPUT_TOOL: &str = "ptah_resolve_work_input";
pub const FUTURE_LIST_EXECUTION_INTENTS_TOOL: &str = "ptah_list_execution_intents";

pub const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PROBES: usize = 256;
pub const MAX_WORKSPACE_FIXTURES: usize = 16;
pub const MAX_REFERENCED_SCENARIOS: usize = 16;
pub const MAX_REQUIRED_TOOLS: usize = 96;
pub const MAX_REQUIRED_CAPABILITIES: usize = 32;
pub const MAX_ACTIONS: usize = 64;
pub const MAX_TRANSITIONS: usize = 64;
pub const MAX_ORACLES: usize = 64;

const MAX_ID_BYTES: usize = 96;
const MAX_DESCRIPTION_BYTES: usize = 1_024;
const MAX_DURATION_SECONDS: u64 = 24 * 60 * 60;
const MAX_ROUNDS: u32 = 100_000;
const MAX_TOKENS: u64 = 2_000_000;
const MAX_RUN_TOKENS: u64 = 250_000;
const MAX_PROVIDER_REQUESTS: u32 = 800;
const MAX_CONTINUATIONS: u32 = 96;
const MAX_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The checked-in overlay. `include_bytes!` makes schema drift fail in tests
/// without consulting an ambient checkout or network resource.
pub const BUNDLED_MANIFEST: &[u8] = include_bytes!("../campaign.v1.json");
const BUNDLED_CATALOG: &[u8] = include_bytes!("../../persistent-agent-scenarios.v1.json");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignManifest {
    pub schema: String,
    pub campaign_id: String,
    pub catalog: CatalogReference,
    pub campaign_bounds: HardBounds,
    pub workspace_fixtures: Vec<WorkspaceFixture>,
    pub probes: Vec<ProbeDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogReference {
    pub relative_path: String,
    pub schema: String,
    pub catalog_id: String,
    pub catalog_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFixture {
    pub id: String,
    pub kind: WorkspaceFixtureKind,
    pub max_seed_bytes: u64,
    pub disposable_required: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFixtureKind {
    SyntheticDisposable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProbeDefinition {
    pub id: String,
    pub description: String,
    /// Existing catalog scenarios for which this probe supplies supporting
    /// evidence. This does not make the probe a provider capture scenario.
    pub catalog_scenario_ids: Vec<String>,
    pub scope: ProbeScope,
    pub effect: ProbeEffect,
    pub required_tools: Vec<String>,
    pub required_capabilities: Vec<RunnerCapability>,
    pub eligibility: Eligibility,
    pub workspace_fixture: String,
    pub bounds: HardBounds,
    pub actions: Vec<ProbeAction>,
    pub expected_transitions: Vec<TransitionExpectation>,
    pub oracle_codes: Vec<OracleCode>,
    pub missing_requirement: MissingRequirementDisposition,
    pub incomplete_evidence: IncompleteEvidenceDisposition,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProbeScope {
    ServiceContract,
    ProviderStructural,
    NativeManagedFuture,
    Soak,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProbeEffect {
    ReadOnly,
    Mutating,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Eligibility {
    pub offline: bool,
    pub live: bool,
    pub local_launch: bool,
    pub attach: bool,
    pub attach_access: AttachAccess,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AttachAccess {
    NotAllowed,
    ReadOnly,
    ExplicitMutationOptIn,
}

/// Runner-owned facilities, distinct from server-advertised MCP tools.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RunnerCapability {
    McpStreamableHttp,
    DisposableWorkspace,
    ClientReconnect,
    LocalRestartControl,
    DeterministicClock,
    LiveProviderRoute,
    ProviderObservation,
    AuthoritativeUsage,
    PersistentAgentEvidence,
    PreexistingTestAgents,
    ExplicitPermissionControl,
    NativeManagedExecution,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissingRequirementDisposition {
    SkipAsUnsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteEvidenceDisposition {
    NotApplicable,
    MarkIndeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HardBounds {
    pub bound_profile: BoundProfile,
    pub max_duration_seconds: u64,
    pub max_rounds: u32,
    pub max_run_tokens: u64,
    pub max_total_tokens: u64,
    pub max_provider_requests: u32,
    pub max_continuations: u32,
    pub max_response_bytes_per_request: u64,
    pub max_output_bytes: u64,
    pub max_artifact_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProbeAction {
    DiscoverCapabilities,
    CreatePersistentAgent,
    CreateTestAgents,
    SubmitBoundedRun,
    WaitForTerminalRun,
    SteerRun,
    CancelRun,
    DisconnectClient,
    ReconnectClient,
    RestartService,
    InspectCheckpoint,
    ResumePersistentAgent,
    CreateWork,
    InspectWork,
    InspectWorkSet,
    InspectWorkAttempts,
    InspectRunSet,
    AssignWork,
    OfferWork,
    AcceptWork,
    ClaimWork,
    HeartbeatLease,
    ReportWorkProgress,
    CompleteWork,
    ExpireLease,
    RetryWork,
    CreateDependency,
    CompleteDependency,
    RequestApproval,
    ApproveWork,
    CancelWork,
    ReplayRequest,
    ReplayChangedPayload,
    CreateRoutine,
    FireRoutine,
    AdvanceDeterministicClock,
    PauseRoutine,
    DisableRoutine,
    EnableRoutine,
    InspectActivations,
    DiscoverWorkers,
    HeartbeatWorker,
    CreateChildWork,
    SendQuestion,
    SendAnswer,
    SendHandoff,
    SendReview,
    ReadInboxCursor,
    ExpireMessage,
    AttemptCrossWorkspaceMutation,
    SubmitManagedWork,
    SubmitIndependentPermissionCases,
    WaitForManagedAdmission,
    ObservePermissionPark,
    AllowPermission,
    DenyPermission,
    InterruptRun,
    InspectExecutionIntent,
    RepeatBoundedCycle,
    SnapshotInvariants,
    InspectManagedPolicy,
    AuthorizeWorkExecution,
    AttemptInvalidWorkInputResolution,
    ObserveNativeTicks,
    ClaimRetriedWork,
    FailWork,
    CreateManagerPlan,
    InspectManagerPlan,
    AdvanceManagerPlan,
    TickManagerPlan,
    ReplanManagerPlan,
}

impl ProbeAction {
    pub fn required_tools(self) -> &'static [&'static str] {
        match self {
            Self::DiscoverCapabilities => &["ptah_get_capacity", "ptah_list_sessions"],
            Self::CreatePersistentAgent => &[
                "ptah_create_session",
                "ptah_submit_task",
                "ptah_cancel",
                "ptah_get_run",
                "ptah_list_persistent_agents",
            ],
            Self::CreateTestAgents => &[
                "ptah_create_session",
                "ptah_submit_task",
                "ptah_cancel",
                "ptah_get_run",
                "ptah_list_persistent_agents",
            ],
            Self::SubmitBoundedRun => &["ptah_submit_task"],
            Self::WaitForTerminalRun => &["ptah_get_run"],
            Self::SteerRun => &["ptah_steer"],
            Self::CancelRun | Self::InterruptRun => &["ptah_cancel"],
            Self::DisconnectClient | Self::ReconnectClient | Self::RestartService => &[],
            Self::InspectCheckpoint => &["ptah_get_persistent_agent", "ptah_get_run"],
            Self::ResumePersistentAgent => &["ptah_resume_persistent_agent"],
            Self::CreateWork | Self::CreateDependency | Self::CreateChildWork => {
                &["ptah_create_work"]
            }
            Self::FailWork => &["ptah_fail_work"],
            Self::CreateManagerPlan => &["ptah_create_manager_plan"],
            Self::InspectManagerPlan => &["ptah_get_manager_plan"],
            Self::AdvanceManagerPlan => &["ptah_advance_manager_plan"],
            Self::TickManagerPlan => &["ptah_tick_manager_plan"],
            Self::ReplanManagerPlan => &["ptah_replan_manager_plan"],
            Self::InspectWork => &["ptah_get_work"],
            Self::InspectWorkSet => &["ptah_list_work"],
            Self::InspectWorkAttempts => &["ptah_get_work"],
            Self::InspectRunSet => &["ptah_list_runs"],
            Self::AssignWork => &["ptah_assign_work"],
            Self::OfferWork => &["ptah_offer_work"],
            Self::AcceptWork => &["ptah_accept_work"],
            Self::ClaimWork => &["ptah_claim_work"],
            Self::HeartbeatLease => &["ptah_renew_work"],
            Self::ReportWorkProgress => &["ptah_report_work_progress"],
            Self::CompleteWork | Self::CompleteDependency => &["ptah_complete_work"],
            Self::ExpireLease => &["ptah_get_work"],
            Self::RetryWork => &["ptah_retry_work"],
            Self::RequestApproval => &["ptah_create_work"],
            Self::ApproveWork => &["ptah_approve_work"],
            Self::CancelWork => &["ptah_cancel_work"],
            Self::ReplayRequest | Self::ReplayChangedPayload => &[],
            Self::CreateRoutine => &["ptah_create_routine"],
            Self::FireRoutine => &["ptah_fire_routine"],
            Self::AdvanceDeterministicClock => &[],
            Self::PauseRoutine => &["ptah_pause_routine"],
            Self::DisableRoutine => &["ptah_disable_routine"],
            Self::EnableRoutine => &["ptah_enable_routine"],
            Self::InspectActivations => &["ptah_list_activations"],
            Self::DiscoverWorkers => &["ptah_list_workers"],
            Self::HeartbeatWorker => &["ptah_heartbeat_worker"],
            Self::SendQuestion | Self::SendAnswer | Self::SendHandoff | Self::SendReview => {
                &["ptah_send_message"]
            }
            Self::ReadInboxCursor | Self::ExpireMessage => &["ptah_list_inbox"],
            Self::AttemptCrossWorkspaceMutation => &["ptah_create_work", "ptah_send_message"],
            Self::SubmitManagedWork | Self::SubmitIndependentPermissionCases => &[
                "ptah_create_session",
                "ptah_submit_task",
                "ptah_cancel",
                "ptah_get_run",
                "ptah_list_persistent_agents",
                FUTURE_SET_MANAGED_EXECUTION_TOOL,
                "ptah_create_work",
                "ptah_assign_work",
            ],
            Self::WaitForManagedAdmission => &[
                "ptah_get_work",
                "ptah_get_run",
                FUTURE_LIST_EXECUTION_INTENTS_TOOL,
            ],
            Self::ObservePermissionPark => &[
                FUTURE_LIST_EXECUTION_INTENTS_TOOL,
                "ptah_list_inbox",
                "ptah_get_work",
            ],
            Self::AllowPermission | Self::DenyPermission => &[FUTURE_RESOLVE_WORK_INPUT_TOOL],
            Self::InspectExecutionIntent => &[FUTURE_LIST_EXECUTION_INTENTS_TOOL],
            Self::RepeatBoundedCycle => &[
                "ptah_create_work",
                "ptah_complete_work",
                "ptah_create_routine",
                "ptah_fire_routine",
                "ptah_send_message",
            ],
            Self::SnapshotInvariants => &[
                "ptah_list_work",
                "ptah_list_routines",
                "ptah_list_activations",
                "ptah_list_inbox",
            ],
            Self::InspectManagedPolicy => &[FUTURE_GET_MANAGED_EXECUTION_TOOL, "ptah_get_capacity"],
            Self::AuthorizeWorkExecution => &[FUTURE_AUTHORIZE_WORK_EXECUTION_TOOL],
            Self::AttemptInvalidWorkInputResolution => &[
                FUTURE_RESOLVE_WORK_INPUT_TOOL,
                FUTURE_LIST_EXECUTION_INTENTS_TOOL,
                "ptah_get_work",
            ],
            Self::ObserveNativeTicks => &["ptah_get_capacity"],
            Self::ClaimRetriedWork => &["ptah_claim_work"],
        }
    }

    fn mutates(self) -> bool {
        !matches!(
            self,
            Self::DiscoverCapabilities
                | Self::WaitForTerminalRun
                | Self::DisconnectClient
                | Self::ReconnectClient
                | Self::RestartService
                | Self::InspectCheckpoint
                | Self::ResumePersistentAgent
                | Self::InspectWork
                | Self::InspectWorkSet
                | Self::InspectWorkAttempts
                | Self::InspectRunSet
                | Self::ExpireLease
                | Self::InspectActivations
                | Self::DiscoverWorkers
                | Self::ReadInboxCursor
                | Self::ExpireMessage
                | Self::ObservePermissionPark
                | Self::InspectExecutionIntent
                | Self::SnapshotInvariants
                | Self::InspectManagedPolicy
                | Self::ObserveNativeTicks
                | Self::WaitForManagedAdmission
                | Self::InspectManagerPlan
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct TransitionExpectation {
    pub entity: DurableEntity,
    pub from: DurableState,
    pub to: DurableState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DurableEntity {
    Campaign,
    Service,
    Agent,
    Run,
    Work,
    Attempt,
    Lease,
    Routine,
    Activation,
    Worker,
    Message,
    Cursor,
    Permission,
    ExecutionIntent,
    ManagerPlan,
    ManagerStep,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DurableState {
    Absent,
    Starting,
    Ready,
    Created,
    Queued,
    Offered,
    Accepted,
    Claimed,
    Leased,
    Admitted,
    Active,
    Running,
    Blocked,
    AwaitingApproval,
    Enabled,
    Paused,
    Disabled,
    Parked,
    Allowed,
    Denied,
    Retrying,
    Recovered,
    Finalized,
    Advanced,
    Deduplicated,
    Expired,
    Acknowledged,
    Completed,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    NeedsReplan,
    Superseded,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OracleCode {
    ServiceReady,
    RequiredToolsDiscovered,
    AgentProjectionStable,
    RunTerminalAndBounded,
    SteeringObserved,
    CancellationTerminal,
    EventCursorMonotonic,
    EventCursorNoGap,
    DurableReadAfterRestart,
    NoImplicitInvocationResume,
    CheckpointInspectable,
    WorkLifecycleOrdered,
    LeaseExpirySingleRetry,
    DependencyBlocksThenUnblocks,
    ApprovalGateEnforced,
    RequestReplaySameResource,
    ChangedPayloadConflict,
    ManualActivationCreated,
    ScheduledActivationAtMostOnce,
    ActivationDeduplicated,
    RoutineLifecycleEnforced,
    ActivationRecoveredAfterRestart,
    WorkerProjectionStable,
    WorkerHeartbeatFresh,
    ParentChildLineagePreserved,
    TypedMessagesRoundTrip,
    InboxCursorMonotonic,
    MessageReplayDeduplicated,
    MessageExpiryEnforced,
    CrossWorkspaceRejected,
    ManagedPolicyDefaultOff,
    NativeRunLinkedToWork,
    PermissionDecisionExplicit,
    NoDuplicateNativeRun,
    PermissionBoundToExactRun,
    IndependentPermissionCasesObserved,
    InvalidPermissionResolutionFailsClosed,
    WorkRemainsParkedOnInputMismatch,
    ExternalStateConverged,
    ManualRetryRemainsExternal,
    RetryIneligibleNativeReadmissionBlocked,
    OriginalFailedAttemptInspectable,
    SingleAttemptRunLinkage,
    CycleCountBounded,
    GrowthWithinArtifactBudget,
    NoLiveLeaseLeak,
    NoLiveCursorLeak,
    RestartReconnectObserved,
    ContinuationRunCreated,
    CheckpointUsedForContinuation,
    ResumeRequestReplayStable,
    ManagerContainerNotExecutable,
    ManagerAdvanceRespectsDependencies,
    ManagerNotificationFencedByWorkRevision,
    ManagerStalePlanRevisionRejected,
    ManagerReplanSupersedesAndSucceeds,
}

#[derive(Debug, Deserialize)]
struct CatalogProjection {
    schema: String,
    catalog_id: String,
    catalog_version: u32,
    scenarios: Vec<CatalogScenarioProjection>,
}

#[derive(Debug, Deserialize)]
struct CatalogScenarioProjection {
    id: String,
}

impl CampaignManifest {
    /// Parse the overlay and fail closed against the compiled-in authoritative
    /// v1 catalog. [`Self::load_checked`] additionally verifies the catalog
    /// file beneath the selected repository root.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_MANIFEST_BYTES {
            bail!("campaign manifest byte length is outside its bound");
        }
        let manifest: Self =
            serde_json::from_slice(bytes).context("parse campaign manifest JSON")?;
        manifest.validate_shape()?;
        let value = serde_json::to_value(&manifest)?;
        scan_value_for_forbidden_data(&value)
            .map_err(|_| anyhow::anyhow!("campaign manifest failed forbidden-data scanning"))?;
        manifest.validate_against_catalog_slice(BUNDLED_CATALOG)?;
        Ok(manifest)
    }

    /// Load the repository-bundled overlay and check every catalog reference
    /// against the authoritative bundled catalog.
    pub fn bundled() -> Result<Self> {
        let manifest = Self::from_slice(BUNDLED_MANIFEST)?;
        manifest.validate_against_catalog_slice(BUNDLED_CATALOG)?;
        Ok(manifest)
    }

    /// Load an overlay and its declared catalog beneath an explicit repository
    /// root. Canonical containment and symlink rejection prevent an overlay
    /// from redirecting validation to an unrelated file.
    pub fn load_checked(manifest_path: &Path, repository_root: &Path) -> Result<Self> {
        reject_symlink_components(repository_root, manifest_path)
            .context("campaign manifest path")?;
        let root = dunce::canonicalize(repository_root).context("canonicalize repository root")?;
        let manifest_path =
            dunce::canonicalize(manifest_path).context("canonicalize campaign manifest path")?;
        if !manifest_path.starts_with(&root) {
            bail!("campaign manifest escapes repository root");
        }
        let manifest_bytes =
            read_bounded(&manifest_path, MAX_MANIFEST_BYTES).context("read campaign manifest")?;
        let manifest = Self::from_slice(&manifest_bytes)?;

        let parent = manifest_path
            .parent()
            .context("campaign manifest has no parent directory")?;
        let catalog_path = parent.join(&manifest.catalog.relative_path);
        reject_symlink(&catalog_path).context("catalog path")?;
        let catalog_path =
            dunce::canonicalize(&catalog_path).context("canonicalize catalog path")?;
        if !catalog_path.starts_with(&root) {
            bail!("campaign catalog escapes repository root");
        }
        let catalog_bytes =
            read_bounded(&catalog_path, MAX_CATALOG_BYTES).context("read campaign catalog")?;
        manifest.validate_against_catalog_slice(&catalog_bytes)?;
        Ok(manifest)
    }

    pub fn probe(&self, id: &str) -> Option<&ProbeDefinition> {
        self.probes.iter().find(|probe| probe.id == id)
    }

    /// Resolve an explicit CLI/profile selection while rejecting duplicates
    /// and unknown subordinate probe IDs. An empty selection means all probes.
    pub fn select_probes<'a>(&'a self, ids: &[String]) -> Result<Vec<&'a ProbeDefinition>> {
        if ids.is_empty() {
            return Ok(self.probes.iter().collect());
        }
        if ids.len() > MAX_PROBES {
            bail!("selected probe count exceeds its bound");
        }
        let mut selected = Vec::with_capacity(ids.len());
        let mut unique = HashSet::new();
        for id in ids {
            validate_safe_id(id).context("selected probe id")?;
            if !unique.insert(id.as_str()) {
                bail!("probe selection contains a duplicate probe id");
            }
            selected.push(
                self.probe(id)
                    .context("probe selection contains an unknown probe id")?,
            );
        }
        Ok(selected)
    }

    pub fn validate_against_catalog_slice(&self, bytes: &[u8]) -> Result<()> {
        self.validate_shape()?;
        if bytes.is_empty() || bytes.len() > MAX_CATALOG_BYTES {
            bail!("authoritative catalog byte length is outside its bound");
        }
        let catalog: CatalogProjection =
            serde_json::from_slice(bytes).context("parse authoritative scenario catalog")?;
        if catalog.schema != self.catalog.schema
            || catalog.catalog_id != self.catalog.catalog_id
            || catalog.catalog_version != self.catalog.catalog_version
        {
            bail!("authoritative scenario catalog identity does not match overlay reference");
        }
        if catalog.scenarios.is_empty() || catalog.scenarios.len() > MAX_PROBES {
            bail!("authoritative scenario catalog count is outside its bound");
        }
        let mut catalog_ids = BTreeSet::new();
        for scenario in catalog.scenarios {
            validate_safe_id(&scenario.id).context("catalog scenario id")?;
            if !catalog_ids.insert(scenario.id) {
                bail!("authoritative scenario catalog contains a duplicate scenario id");
            }
        }
        for probe in &self.probes {
            for scenario_id in &probe.catalog_scenario_ids {
                if !catalog_ids.contains(scenario_id) {
                    bail!("probe references an unknown authoritative catalog scenario id");
                }
            }
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        if self.schema != LAB_MANIFEST_SCHEMA {
            bail!("unsupported campaign manifest schema");
        }
        validate_safe_id(&self.campaign_id).context("campaign_id")?;
        self.catalog.validate()?;
        self.campaign_bounds.validate().context("campaign_bounds")?;
        if self.workspace_fixtures.is_empty()
            || self.workspace_fixtures.len() > MAX_WORKSPACE_FIXTURES
        {
            bail!("workspace fixture count is outside its bound");
        }
        if self.probes.is_empty() || self.probes.len() > MAX_PROBES {
            bail!("probe count is outside its bound");
        }

        let mut fixture_ids = HashSet::new();
        for fixture in &self.workspace_fixtures {
            fixture.validate()?;
            if !fixture_ids.insert(fixture.id.as_str()) {
                bail!("campaign manifest contains a duplicate workspace fixture id");
            }
        }

        let mut probe_ids = HashSet::new();
        for probe in &self.probes {
            probe.validate(&self.campaign_bounds, &fixture_ids)?;
            if !probe_ids.insert(probe.id.as_str()) {
                bail!("campaign manifest contains a duplicate probe id");
            }
        }
        Ok(())
    }
}

impl CatalogReference {
    fn validate(&self) -> Result<()> {
        if self.relative_path != AUTHORITATIVE_CATALOG_RELATIVE_PATH
            || self.schema != AUTHORITATIVE_CATALOG_SCHEMA
            || self.catalog_id != AUTHORITATIVE_CATALOG_ID
            || self.catalog_version != AUTHORITATIVE_CATALOG_VERSION
        {
            bail!("campaign manifest must reference the authoritative catalog identity");
        }
        validate_relative_path(&self.relative_path)?;
        Ok(())
    }
}

impl WorkspaceFixture {
    fn validate(&self) -> Result<()> {
        validate_safe_id(&self.id).context("workspace fixture id")?;
        if self.max_seed_bytes == 0 || self.max_seed_bytes > MAX_OUTPUT_BYTES {
            bail!("workspace fixture seed byte bound is invalid");
        }
        if !self.disposable_required {
            bail!("certification workspace fixtures must be disposable");
        }
        Ok(())
    }
}

impl ProbeDefinition {
    fn validate(&self, campaign_bounds: &HardBounds, fixture_ids: &HashSet<&str>) -> Result<()> {
        validate_safe_id(&self.id).context("probe id")?;
        validate_description(&self.description).context("probe description")?;
        if self.catalog_scenario_ids.is_empty()
            || self.catalog_scenario_ids.len() > MAX_REFERENCED_SCENARIOS
        {
            bail!("probe catalog reference count is outside its bound");
        }
        ensure_unique_strings(&self.catalog_scenario_ids, "catalog scenario id")?;
        for scenario_id in &self.catalog_scenario_ids {
            validate_safe_id(scenario_id).context("probe catalog scenario id")?;
        }
        if self.required_tools.len() > MAX_REQUIRED_TOOLS {
            bail!("probe required tool count exceeds its bound");
        }
        ensure_unique_strings(&self.required_tools, "required MCP tool")?;
        for tool in &self.required_tools {
            validate_tool_name(tool)?;
        }
        ensure_unique_copy(
            &self.required_capabilities,
            MAX_REQUIRED_CAPABILITIES,
            "runner capability",
        )?;
        self.eligibility.validate(self.effect)?;
        validate_safe_id(&self.workspace_fixture).context("probe workspace fixture")?;
        if !fixture_ids.contains(self.workspace_fixture.as_str()) {
            bail!("probe references an unknown workspace fixture");
        }
        self.bounds.validate().context("probe bounds")?;
        self.bounds.validate_within(campaign_bounds)?;
        ensure_unique_copy(&self.actions, MAX_ACTIONS, "probe action")?;
        if self.actions.is_empty() {
            bail!("probe must declare at least one action");
        }
        ensure_unique_copy(
            &self.expected_transitions,
            MAX_TRANSITIONS,
            "expected transition",
        )?;
        if self.expected_transitions.is_empty() {
            bail!("probe must declare at least one expected transition");
        }
        ensure_unique_copy(&self.oracle_codes, MAX_ORACLES, "oracle code")?;
        if self.oracle_codes.is_empty() {
            bail!("probe must declare at least one oracle code");
        }

        let capabilities: HashSet<_> = self.required_capabilities.iter().copied().collect();
        let actions: HashSet<_> = self.actions.iter().copied().collect();
        let oracles: HashSet<_> = self.oracle_codes.iter().copied().collect();
        let has_transition = |entity, from, to| {
            self.expected_transitions.iter().any(|transition| {
                transition.entity == entity && transition.from == from && transition.to == to
            })
        };
        if self.scope == ProbeScope::NativeManagedFuture
            && !capabilities.contains(&RunnerCapability::NativeManagedExecution)
        {
            bail!("native managed execution probes must be capability gated");
        }
        if (actions.contains(&ProbeAction::AdvanceDeterministicClock)
            || actions.contains(&ProbeAction::ExpireMessage))
            && !capabilities.contains(&RunnerCapability::DeterministicClock)
        {
            bail!("clock-dependent probes must require deterministic_clock");
        }
        if actions.contains(&ProbeAction::RestartService)
            && (!capabilities.contains(&RunnerCapability::LocalRestartControl)
                || !self.eligibility.local_launch)
        {
            bail!("restart probes must require owned local restart control");
        }
        if (actions.contains(&ProbeAction::DisconnectClient)
            || actions.contains(&ProbeAction::ReconnectClient))
            && !capabilities.contains(&RunnerCapability::ClientReconnect)
        {
            bail!("disconnect/reconnect probes must require client_reconnect");
        }
        if self.scope == ProbeScope::ProviderStructural
            && (!self.eligibility.live
                || self.eligibility.offline
                || !capabilities.contains(&RunnerCapability::LiveProviderRoute)
                || !capabilities.contains(&RunnerCapability::ProviderObservation)
                || !capabilities.contains(&RunnerCapability::AuthoritativeUsage))
        {
            bail!("provider structural probes require live fail-closed observation");
        }
        let submits_managed_work = actions.contains(&ProbeAction::SubmitManagedWork)
            || actions.contains(&ProbeAction::SubmitIndependentPermissionCases);
        if self.scope == ProbeScope::NativeManagedFuture
            && submits_managed_work
            && (self.eligibility.offline
                || !self.eligibility.live
                || !capabilities.contains(&RunnerCapability::LiveProviderRoute)
                || !capabilities.contains(&RunnerCapability::ProviderObservation)
                || !capabilities.contains(&RunnerCapability::AuthoritativeUsage))
        {
            bail!("managed native execution probes require live fail-closed observation");
        }
        if actions.contains(&ProbeAction::AllowPermission)
            || actions.contains(&ProbeAction::DenyPermission)
        {
            for action in [
                ProbeAction::SubmitIndependentPermissionCases,
                ProbeAction::AuthorizeWorkExecution,
                ProbeAction::ObservePermissionPark,
                ProbeAction::AttemptInvalidWorkInputResolution,
                ProbeAction::AllowPermission,
                ProbeAction::DenyPermission,
                ProbeAction::InspectWorkAttempts,
                ProbeAction::InspectRunSet,
            ] {
                if !actions.contains(&action) {
                    bail!("permission probe lacks its two-case public evidence action");
                }
            }
            for oracle in [
                OracleCode::PermissionDecisionExplicit,
                OracleCode::PermissionBoundToExactRun,
                OracleCode::IndependentPermissionCasesObserved,
                OracleCode::InvalidPermissionResolutionFailsClosed,
                OracleCode::WorkRemainsParkedOnInputMismatch,
            ] {
                if !oracles.contains(&oracle) {
                    bail!("permission probe lacks its two-case fail-closed oracle");
                }
            }
        }
        if self.scope == ProbeScope::NativeManagedFuture
            && actions.contains(&ProbeAction::RetryWork)
        {
            if actions.contains(&ProbeAction::InterruptRun) {
                bail!("retry-ineligible native proof must use a failed Run, not interruption");
            }
            let restart_induced = actions.contains(&ProbeAction::RestartService);
            for action in [
                ProbeAction::SubmitManagedWork,
                ProbeAction::WaitForTerminalRun,
                ProbeAction::InspectWorkAttempts,
                ProbeAction::RetryWork,
                ProbeAction::ObserveNativeTicks,
                ProbeAction::InspectExecutionIntent,
                ProbeAction::InspectRunSet,
                ProbeAction::InspectWorkSet,
                ProbeAction::ClaimRetriedWork,
                ProbeAction::InspectWork,
            ] {
                if !actions.contains(&action) {
                    bail!("retry-ineligible native probe lacks public evidence actions");
                }
            }
            let terminal_run_transition = if restart_induced {
                (
                    DurableEntity::Run,
                    DurableState::Running,
                    DurableState::Interrupted,
                )
            } else {
                (
                    DurableEntity::Run,
                    DurableState::Running,
                    DurableState::Failed,
                )
            };
            let terminal_attempt_transition = if restart_induced {
                (
                    DurableEntity::Attempt,
                    DurableState::Running,
                    DurableState::Expired,
                )
            } else {
                (
                    DurableEntity::Attempt,
                    DurableState::Running,
                    DurableState::Failed,
                )
            };
            for (entity, from, to) in [
                terminal_run_transition,
                terminal_attempt_transition,
                (
                    DurableEntity::ExecutionIntent,
                    DurableState::Admitted,
                    DurableState::Finalized,
                ),
                (
                    DurableEntity::Work,
                    DurableState::Running,
                    DurableState::Failed,
                ),
                (
                    DurableEntity::Work,
                    DurableState::Failed,
                    DurableState::Queued,
                ),
                (
                    DurableEntity::Work,
                    DurableState::Queued,
                    DurableState::Leased,
                ),
            ] {
                if !has_transition(entity, from, to) {
                    bail!("retry-ineligible native probe declares an incorrect state flow");
                }
            }
            for oracle in [
                OracleCode::RetryIneligibleNativeReadmissionBlocked,
                OracleCode::ManualRetryRemainsExternal,
                OracleCode::OriginalFailedAttemptInspectable,
                OracleCode::NoDuplicateNativeRun,
            ] {
                if !oracles.contains(&oracle) {
                    bail!("retry-ineligible native probe lacks a required oracle");
                }
            }
        }
        if self.scope == ProbeScope::NativeManagedFuture
            && actions.contains(&ProbeAction::RestartService)
        {
            for action in [
                ProbeAction::SubmitManagedWork,
                ProbeAction::WaitForManagedAdmission,
                ProbeAction::ObserveNativeTicks,
                ProbeAction::InspectWorkAttempts,
                ProbeAction::InspectRunSet,
                ProbeAction::InspectExecutionIntent,
            ] {
                if !actions.contains(&action) {
                    bail!("native restart probe lacks externally observable evidence actions");
                }
            }
            for (entity, from, to) in [
                (
                    DurableEntity::Run,
                    DurableState::Running,
                    DurableState::Interrupted,
                ),
                (
                    DurableEntity::Attempt,
                    DurableState::Running,
                    DurableState::Expired,
                ),
                (
                    DurableEntity::Work,
                    DurableState::Running,
                    DurableState::Failed,
                ),
                (
                    DurableEntity::ExecutionIntent,
                    DurableState::Admitted,
                    DurableState::Finalized,
                ),
            ] {
                if !has_transition(entity, from, to) {
                    bail!("native restart probe overclaims or omits public convergence");
                }
            }
            if self
                .expected_transitions
                .iter()
                .any(|transition| transition.to == DurableState::Recovered)
            {
                bail!("native restart probe cannot claim an internal recovered state");
            }
            for oracle in [
                OracleCode::ExternalStateConverged,
                OracleCode::NoDuplicateNativeRun,
                OracleCode::RestartReconnectObserved,
                OracleCode::NoImplicitInvocationResume,
            ] {
                if !oracles.contains(&oracle) {
                    bail!("native restart probe lacks a public convergence oracle");
                }
            }
        }
        if self.scope == ProbeScope::NativeManagedFuture
            && actions.contains(&ProbeAction::ReplayRequest)
            && oracles.contains(&OracleCode::NoDuplicateNativeRun)
        {
            for action in [
                ProbeAction::SubmitManagedWork,
                ProbeAction::WaitForManagedAdmission,
                ProbeAction::ObserveNativeTicks,
                ProbeAction::InspectWorkSet,
                ProbeAction::InspectWorkAttempts,
                ProbeAction::InspectRunSet,
                ProbeAction::InspectExecutionIntent,
            ] {
                if !actions.contains(&action) {
                    bail!("native no-duplicate probe lacks public count/link evidence actions");
                }
            }
            for (entity, from, to) in [
                (
                    DurableEntity::ExecutionIntent,
                    DurableState::Absent,
                    DurableState::Admitted,
                ),
                (
                    DurableEntity::Attempt,
                    DurableState::Absent,
                    DurableState::Leased,
                ),
                (
                    DurableEntity::Run,
                    DurableState::Absent,
                    DurableState::Queued,
                ),
            ] {
                if !has_transition(entity, from, to) {
                    bail!("native no-duplicate probe lacks observable admission transitions");
                }
            }
            if self.expected_transitions.iter().any(|transition| {
                transition.entity == DurableEntity::Run
                    && transition.to == DurableState::Deduplicated
            }) || !oracles.contains(&OracleCode::SingleAttemptRunLinkage)
            {
                bail!("native no-duplicate probe relies on a non-public deduplicated Run state");
            }
        }
        match (self.scope, self.incomplete_evidence) {
            (
                ProbeScope::ProviderStructural | ProbeScope::NativeManagedFuture,
                IncompleteEvidenceDisposition::MarkIndeterminate,
            )
            | (
                ProbeScope::ServiceContract | ProbeScope::Soak,
                IncompleteEvidenceDisposition::NotApplicable,
            ) => {}
            _ => bail!("incomplete evidence disposition is inconsistent with probe scope"),
        }
        for action in &self.actions {
            for required in action.required_tools() {
                if !self.required_tools.iter().any(|tool| tool == required) {
                    bail!("probe action is missing its required MCP tool declaration");
                }
            }
        }
        validate_action_sequence(self, &actions)?;
        Ok(())
    }
}

impl Eligibility {
    fn validate(&self, effect: ProbeEffect) -> Result<()> {
        if !self.offline && !self.live {
            bail!("probe must be eligible for at least one runtime mode");
        }
        if !self.local_launch && !self.attach {
            bail!("probe must be eligible for at least one transport mode");
        }
        match (self.attach, effect, self.attach_access) {
            (false, _, AttachAccess::NotAllowed)
            | (true, ProbeEffect::ReadOnly, AttachAccess::ReadOnly)
            | (true, ProbeEffect::Mutating, AttachAccess::ExplicitMutationOptIn) => Ok(()),
            _ => bail!("attach access is inconsistent with probe eligibility/effect"),
        }
    }
}

impl HardBounds {
    fn validate(&self) -> Result<()> {
        let profile = self.bound_profile.limits();
        if self.max_duration_seconds == 0 || self.max_duration_seconds > MAX_DURATION_SECONDS {
            bail!("max_duration_seconds is outside its hard bound");
        }
        if self.max_rounds == 0 || self.max_rounds > MAX_ROUNDS {
            bail!("max_rounds is outside its hard bound");
        }
        if self.max_run_tokens == 0 || self.max_run_tokens > MAX_RUN_TOKENS {
            bail!("max_run_tokens is outside its hard bound");
        }
        if self.max_total_tokens == 0
            || self.max_total_tokens > MAX_TOKENS
            || self.max_run_tokens > self.max_total_tokens
        {
            bail!("max_total_tokens is outside its hard bound");
        }
        if self.max_provider_requests == 0
            || self.max_provider_requests > MAX_PROVIDER_REQUESTS
            || self.max_continuations == 0
            || self.max_continuations > MAX_CONTINUATIONS
        {
            bail!("provider request or continuation ceiling is outside its hard bound");
        }
        if self.max_response_bytes_per_request == 0
            || self.max_response_bytes_per_request > MAX_OUTPUT_BYTES
        {
            bail!("response byte ceiling is outside its hard bound");
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > MAX_OUTPUT_BYTES {
            bail!("max_output_bytes is outside its hard bound");
        }
        if self.max_artifact_bytes == 0 || self.max_artifact_bytes > MAX_ARTIFACT_BYTES {
            bail!("max_artifact_bytes is outside its hard bound");
        }
        if self.max_output_bytes > self.max_artifact_bytes {
            bail!("max_output_bytes must not exceed max_artifact_bytes");
        }
        if self.max_run_tokens > profile.per_run_tokens
            || self.max_total_tokens > profile.campaign_tokens
            || self.max_provider_requests > profile.provider_requests
            || self.max_continuations > profile.continuations
            || self.max_duration_seconds > profile.duration_seconds
            || self.max_artifact_bytes > profile.artifact_bytes
            || self.max_response_bytes_per_request > profile.response_bytes
        {
            bail!("bounds exceed the declared authoritative catalog profile");
        }
        Ok(())
    }

    fn validate_within(&self, outer: &Self) -> Result<()> {
        if self.max_duration_seconds > outer.max_duration_seconds
            || self.max_rounds > outer.max_rounds
            || self.max_run_tokens > outer.max_run_tokens
            || self.max_total_tokens > outer.max_total_tokens
            || self.max_provider_requests > outer.max_provider_requests
            || self.max_continuations > outer.max_continuations
            || self.max_response_bytes_per_request > outer.max_response_bytes_per_request
            || self.max_output_bytes > outer.max_output_bytes
            || self.max_artifact_bytes > outer.max_artifact_bytes
        {
            bail!("probe bounds exceed campaign bounds");
        }
        Ok(())
    }
}

fn validate_action_sequence(probe: &ProbeDefinition, actions: &HashSet<ProbeAction>) -> Result<()> {
    if probe.effect == ProbeEffect::ReadOnly && actions.iter().any(|action| action.mutates()) {
        bail!("read-only probe declares a mutating action");
    }
    let position = |needle: ProbeAction| probe.actions.iter().position(|action| *action == needle);
    let setup_at = [
        ProbeAction::CreatePersistentAgent,
        ProbeAction::CreateTestAgents,
        ProbeAction::SubmitManagedWork,
        ProbeAction::SubmitIndependentPermissionCases,
    ]
    .into_iter()
    .filter_map(position)
    .min();
    for consumer in [
        ProbeAction::CreateWork,
        ProbeAction::CreateDependency,
        ProbeAction::CreateRoutine,
        ProbeAction::DiscoverWorkers,
        ProbeAction::HeartbeatWorker,
        ProbeAction::SendQuestion,
        ProbeAction::SendAnswer,
        ProbeAction::SendHandoff,
        ProbeAction::SendReview,
        ProbeAction::ReadInboxCursor,
        ProbeAction::ExpireMessage,
        ProbeAction::AttemptCrossWorkspaceMutation,
        ProbeAction::RepeatBoundedCycle,
        ProbeAction::InspectManagedPolicy,
        ProbeAction::CreateManagerPlan,
    ] {
        if let Some(consumer_at) = position(consumer) {
            let setup_at = setup_at.context("probe action has no self-contained Agent setup")?;
            if setup_at >= consumer_at {
                bail!("probe action precedes its self-contained Agent setup");
            }
        }
    }
    let submitted_at = position(ProbeAction::SubmitBoundedRun)
        .or_else(|| position(ProbeAction::SubmitManagedWork))
        .or_else(|| position(ProbeAction::SubmitIndependentPermissionCases));
    if let Some(submit_at) = position(ProbeAction::SubmitBoundedRun) {
        let create_at = position(ProbeAction::CreatePersistentAgent)
            .context("bounded Run submission has no self-contained Agent setup")?;
        if create_at >= submit_at {
            bail!("bounded Run submission precedes Agent setup");
        }
    }
    for consumer in [
        ProbeAction::WaitForTerminalRun,
        ProbeAction::SteerRun,
        ProbeAction::CancelRun,
        ProbeAction::InterruptRun,
        ProbeAction::InspectCheckpoint,
        ProbeAction::ResumePersistentAgent,
    ] {
        if let Some(consumer_at) = position(consumer) {
            let Some(submitted_at) = submitted_at else {
                bail!("run-dependent probe action has no self-contained submission");
            };
            if submitted_at >= consumer_at {
                bail!("run-dependent probe action precedes its submission");
            }
        }
    }
    if let Some(resume_at) = position(ProbeAction::ResumePersistentAgent) {
        let checkpoint_at = position(ProbeAction::InspectCheckpoint)
            .context("persistent Agent resume has no checkpoint inspection")?;
        if checkpoint_at >= resume_at {
            bail!("persistent Agent resume precedes checkpoint inspection");
        }
    }
    if let Some(reconnect_at) = position(ProbeAction::ReconnectClient) {
        let disconnect_at = position(ProbeAction::DisconnectClient)
            .context("reconnect action has no preceding disconnect")?;
        if disconnect_at >= reconnect_at {
            bail!("reconnect action precedes its disconnect");
        }
    }
    if let Some(restart_at) = position(ProbeAction::RestartService) {
        let disconnect_at = position(ProbeAction::DisconnectClient)
            .context("restart action has no preceding disconnect")?;
        let reconnect_at = position(ProbeAction::ReconnectClient)
            .context("restart action has no following reconnect")?;
        if disconnect_at >= restart_at || restart_at >= reconnect_at {
            bail!("restart action is not bracketed by disconnect/reconnect");
        }
        if probe.scope == ProbeScope::NativeManagedFuture {
            let admitted_at = position(ProbeAction::WaitForManagedAdmission)
                .context("native restart has no preceding public admission observation")?;
            if admitted_at >= disconnect_at {
                bail!("native restart disconnect precedes admission observation");
            }
        }
    }
    if let Some(inspect_at) = position(ProbeAction::InspectActivations) {
        let create_at = position(ProbeAction::CreateRoutine)
            .context("activation inspection has no self-contained routine setup")?;
        if create_at >= inspect_at {
            bail!("activation inspection precedes routine setup");
        }
    }
    if actions.contains(&ProbeAction::InspectManagedPolicy)
        && probe.scope != ProbeScope::NativeManagedFuture
    {
        bail!("managed policy inspection is reserved for native future probes");
    }
    if let Some(submit_at) = position(ProbeAction::SubmitManagedWork)
        .or_else(|| position(ProbeAction::SubmitIndependentPermissionCases))
    {
        for consumer in [
            ProbeAction::AuthorizeWorkExecution,
            ProbeAction::WaitForManagedAdmission,
            ProbeAction::ObservePermissionPark,
            ProbeAction::InterruptRun,
            ProbeAction::InspectExecutionIntent,
        ] {
            if position(consumer).is_some_and(|consumer_at| consumer_at <= submit_at) {
                bail!("managed execution action precedes composite managed setup");
            }
        }
    }
    if let Some(observe_at) = position(ProbeAction::ObservePermissionPark) {
        let authorize_at = position(ProbeAction::AuthorizeWorkExecution)
            .context("permission park has no pre-execution authorization")?;
        if authorize_at >= observe_at {
            bail!("permission park observation precedes pre-execution authorization");
        }
        for decision in [ProbeAction::AllowPermission, ProbeAction::DenyPermission] {
            if position(decision).is_some_and(|decision_at| decision_at <= observe_at) {
                bail!("permission decision precedes permission park observation");
            }
        }
        if position(ProbeAction::AttemptInvalidWorkInputResolution)
            .is_some_and(|invalid_at| invalid_at <= observe_at)
        {
            bail!("invalid permission resolution precedes permission park observation");
        }
    }
    if probe.scope == ProbeScope::NativeManagedFuture {
        if let Some(retry_at) = position(ProbeAction::RetryWork) {
            let terminal_at = position(ProbeAction::WaitForTerminalRun)
                .context("native Work retry has no preceding terminal Run observation")?;
            let ticks_at = position(ProbeAction::ObserveNativeTicks)
                .context("native Work retry has no following native tick observation")?;
            let attempts_at = position(ProbeAction::InspectWorkAttempts)
                .context("native Work retry has no pre-claim attempt snapshot")?;
            let claim_at = position(ProbeAction::ClaimRetriedWork)
                .context("native Work retry has no following external claim")?;
            let final_work_at = position(ProbeAction::InspectWork)
                .context("native Work retry has no post-claim durable Work read")?;
            if terminal_at >= retry_at
                || retry_at >= ticks_at
                || ticks_at >= attempts_at
                || attempts_at >= claim_at
                || claim_at >= final_work_at
            {
                bail!("retry-ineligible Work actions are not in public state order");
            }
        }
    }
    if actions.contains(&ProbeAction::ReplayRequest)
        && actions.contains(&ProbeAction::ObserveNativeTicks)
        && position(ProbeAction::ReplayRequest) >= position(ProbeAction::ObserveNativeTicks)
    {
        bail!("native tick observation precedes request replay");
    }
    if let Some(reconnect_at) = position(ProbeAction::ReconnectClient) {
        if position(ProbeAction::ObserveNativeTicks)
            .is_some_and(|ticks_at| ticks_at <= reconnect_at)
        {
            bail!("post-restart native observation precedes reconnect");
        }
    }
    if actions.contains(&ProbeAction::AssignWork) && actions.contains(&ProbeAction::OfferWork) {
        bail!("direct assignment and coordinator offer are alternative Work admission paths");
    }
    if actions.contains(&ProbeAction::AcceptWork) && !actions.contains(&ProbeAction::OfferWork) {
        bail!("Work acceptance requires a preceding coordinator offer");
    }
    if actions.contains(&ProbeAction::OfferWork) && !actions.contains(&ProbeAction::AcceptWork) {
        bail!("coordinator-offered Work must declare explicit acceptance");
    }
    if actions.contains(&ProbeAction::ClaimWork)
        && !actions.contains(&ProbeAction::AssignWork)
        && !actions.contains(&ProbeAction::AcceptWork)
        // A manager plan materializes its own child Work, so advancing the
        // plan is that Work's admission path.
        && !actions.contains(&ProbeAction::AdvanceManagerPlan)
    {
        bail!("Work claim has no direct assignment or accepted offer");
    }
    for consumer in [
        ProbeAction::HeartbeatLease,
        ProbeAction::ReportWorkProgress,
        ProbeAction::CompleteWork,
        ProbeAction::ExpireLease,
    ] {
        if actions.contains(&consumer) && !actions.contains(&ProbeAction::ClaimWork) {
            bail!("leased Work action has no preceding claim");
        }
    }
    if actions.contains(&ProbeAction::CreateChildWork)
        && !actions.contains(&ProbeAction::CreateWork)
    {
        bail!("child Work action has no self-contained parent Work");
    }
    if actions.contains(&ProbeAction::CompleteDependency)
        && !actions.contains(&ProbeAction::CreateDependency)
    {
        bail!("dependency completion has no self-contained dependency setup");
    }
    let admission = if actions.contains(&ProbeAction::AssignWork) {
        ProbeAction::AssignWork
    } else {
        ProbeAction::OfferWork
    };
    let ordered_work = [
        ProbeAction::CreateWork,
        admission,
        ProbeAction::AcceptWork,
        ProbeAction::ClaimWork,
        ProbeAction::HeartbeatLease,
        ProbeAction::ReportWorkProgress,
        ProbeAction::CompleteWork,
    ];
    let present: Vec<_> = ordered_work
        .iter()
        .filter_map(|action| position(*action))
        .collect();
    if present.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("Work lifecycle actions are not in durable transition order");
    }
    if let Some(create_at) = position(ProbeAction::CreateRoutine) {
        for consumer in [
            ProbeAction::FireRoutine,
            ProbeAction::PauseRoutine,
            ProbeAction::DisableRoutine,
            ProbeAction::EnableRoutine,
            ProbeAction::InspectActivations,
        ] {
            if position(consumer).is_some_and(|consumer_at| consumer_at <= create_at) {
                bail!("Routine action precedes its self-contained routine setup");
            }
        }
    }
    if actions.contains(&ProbeAction::ReadInboxCursor)
        && !actions.contains(&ProbeAction::SendQuestion)
    {
        bail!("inbox cursor action has no self-contained message setup");
    }
    Ok(())
}

fn validate_safe_id(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ID_BYTES {
        bail!("identifier length is outside its bound");
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        bail!("identifier must start with a lowercase ASCII letter or digit");
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        bail!("identifier must end with a lowercase ASCII letter or digit");
    }
    if bytes
        .iter()
        .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'-')
        || value.contains("--")
    {
        bail!("identifier contains a disallowed character sequence");
    }
    Ok(())
}

fn validate_description(value: &str) -> Result<()> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_DESCRIPTION_BYTES
        || value.chars().any(char::is_control)
    {
        bail!("description is empty, oversized, padded, or contains a control character");
    }
    Ok(())
}

fn validate_tool_name(value: &str) -> Result<()> {
    if value.len() > 128
        || !value.starts_with("ptah_")
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'_')
    {
        bail!("required MCP tool name is invalid");
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute() || value.contains('\\') {
        bail!("catalog reference must be a portable relative path");
    }
    for component in path.components() {
        if matches!(component, Component::RootDir | Component::Prefix(_)) {
            bail!("catalog reference must remain relative");
        }
    }
    Ok(())
}

fn ensure_unique_strings(values: &[String], name: &str) -> Result<()> {
    let mut unique = HashSet::new();
    for value in values {
        if !unique.insert(value.as_str()) {
            bail!("probe contains a duplicate {name}");
        }
    }
    Ok(())
}

fn ensure_unique_copy<T>(values: &[T], max: usize, name: &str) -> Result<()>
where
    T: Copy + Eq + std::hash::Hash,
{
    if values.len() > max {
        bail!("{name} count exceeds its bound");
    }
    let mut unique = HashSet::new();
    for value in values {
        if !unique.insert(*value) {
            bail!("probe contains a duplicate {name}");
        }
    }
    Ok(())
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).context("stat bounded JSON file")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max as u64
    {
        bail!("bounded JSON file size is invalid");
    }
    let mut file = fs::File::open(path).context("open bounded JSON file")?;
    let opened = file.metadata().context("stat opened bounded JSON file")?;
    if !opened.is_file() || !same_file(&metadata, &opened) || opened.len() != metadata.len() {
        bail!("bounded JSON file changed while opening");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read bounded JSON file")?;
    let current = fs::symlink_metadata(path).context("restat bounded JSON file")?;
    if bytes.len() != opened.len() as usize
        || !same_file(&opened, &current)
        || current.len() != opened.len()
    {
        bail!("bounded JSON file changed while reading");
    }
    Ok(bytes)
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)
        .context("stat path without following symlink")?
        .file_type()
        .is_symlink()
    {
        bail!("symlink paths are not accepted");
    }
    Ok(())
}

fn reject_symlink_components(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .context("path is not lexically contained by repository root")?;
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("path contains a traversal component");
        };
        cursor.push(part);
        let metadata = fs::symlink_metadata(&cursor).context("stat path component")?;
        if metadata.file_type().is_symlink() {
            bail!("symlink paths are not accepted");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tempfile::tempdir;

    fn bundled_value() -> Value {
        serde_json::from_slice(BUNDLED_MANIFEST).expect("bundled manifest JSON")
    }

    fn validate_value(value: Value) -> Result<CampaignManifest> {
        let bytes = serde_json::to_vec(&value)?;
        let manifest = CampaignManifest::from_slice(&bytes)?;
        manifest.validate_against_catalog_slice(BUNDLED_CATALOG)?;
        Ok(manifest)
    }

    #[test]
    fn bundled_overlay_validates_and_covers_each_probe_family() {
        let manifest = CampaignManifest::bundled().unwrap();
        assert!(manifest.probes.len() >= 30);
        for prefix in [
            "core-",
            "work-",
            "routine-",
            "coordinator-",
            "native-",
            "soak-",
        ] {
            assert!(manifest
                .probes
                .iter()
                .any(|probe| probe.id.starts_with(prefix)));
        }
    }

    #[test]
    fn bundled_native_probes_use_the_merged_public_tools_and_remain_capability_gated() {
        let manifest = CampaignManifest::bundled().unwrap();
        let known_tools: HashSet<_> = grokptah_agent_bridge::discovered_tool_names()
            .into_iter()
            .collect();
        for tool in [
            FUTURE_SET_MANAGED_EXECUTION_TOOL,
            FUTURE_GET_MANAGED_EXECUTION_TOOL,
            FUTURE_AUTHORIZE_WORK_EXECUTION_TOOL,
            FUTURE_RESOLVE_WORK_INPUT_TOOL,
            FUTURE_LIST_EXECUTION_INTENTS_TOOL,
        ] {
            assert!(
                known_tools.contains(tool),
                "missing merged public tool {tool}"
            );
        }
        let native: Vec<_> = manifest
            .probes
            .iter()
            .filter(|probe| probe.scope == ProbeScope::NativeManagedFuture)
            .collect();
        assert_eq!(native.len(), 6);
        for probe in native {
            assert!(probe
                .required_capabilities
                .contains(&RunnerCapability::NativeManagedExecution));
            assert!(probe
                .required_tools
                .iter()
                .all(|tool| known_tools.contains(tool.as_str())));
        }
    }

    #[test]
    fn unknown_fields_and_schema_fail_closed() {
        let mut value = bundled_value();
        value["unexpected"] = json!(true);
        assert!(CampaignManifest::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut value = bundled_value();
        value["probes"][0]["unexpected"] = json!(true);
        assert!(CampaignManifest::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut value = bundled_value();
        value["schema"] = json!("grokptah.certification_lab_manifest.v999");
        assert!(CampaignManifest::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn duplicate_and_unknown_ids_fail_closed() {
        let mut duplicate_probe = bundled_value();
        duplicate_probe["probes"][1]["id"] = duplicate_probe["probes"][0]["id"].clone();
        assert!(validate_value(duplicate_probe).is_err());

        let mut duplicate_catalog_reference = bundled_value();
        let id = duplicate_catalog_reference["probes"][0]["catalog_scenario_ids"][0].clone();
        duplicate_catalog_reference["probes"][0]["catalog_scenario_ids"] = json!([id.clone(), id]);
        assert!(validate_value(duplicate_catalog_reference).is_err());

        let mut unknown_catalog_reference = bundled_value();
        unknown_catalog_reference["probes"][0]["catalog_scenario_ids"] =
            json!(["not-in-authoritative-catalog-001"]);
        assert!(validate_value(unknown_catalog_reference).is_err());

        let mut unsafe_probe = bundled_value();
        unsafe_probe["probes"][0]["id"] = json!("core-secret=token");
        assert!(validate_value(unsafe_probe).is_err());

        let manifest = CampaignManifest::bundled().unwrap();
        assert!(manifest
            .select_probes(&["unknown-probe-v1".to_string()])
            .is_err());
        assert!(manifest
            .select_probes(&[
                "core-service-readiness-v1".to_string(),
                "core-service-readiness-v1".to_string(),
            ])
            .is_err());
    }

    #[test]
    fn zero_inconsistent_and_over_campaign_bounds_fail_closed() {
        for field in [
            "max_duration_seconds",
            "max_rounds",
            "max_run_tokens",
            "max_total_tokens",
            "max_provider_requests",
            "max_continuations",
            "max_response_bytes_per_request",
            "max_output_bytes",
            "max_artifact_bytes",
        ] {
            let mut value = bundled_value();
            value["probes"][0]["bounds"][field] = json!(0);
            assert!(validate_value(value).is_err(), "field {field}");
        }

        let mut inconsistent = bundled_value();
        inconsistent["probes"][0]["bounds"]["max_output_bytes"] = json!(262_144);
        inconsistent["probes"][0]["bounds"]["max_artifact_bytes"] = json!(131_072);
        assert!(validate_value(inconsistent).is_err());

        let mut over_campaign = bundled_value();
        let campaign_max = over_campaign["campaign_bounds"]["max_rounds"]
            .as_u64()
            .unwrap();
        over_campaign["probes"][0]["bounds"]["max_rounds"] = json!(campaign_max + 1);
        assert!(validate_value(over_campaign).is_err());
    }

    #[test]
    fn clock_dependent_probes_require_deterministic_clock() {
        let mut value = bundled_value();
        let probe = value["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "routine-scheduled-activation-v1")
            .unwrap();
        probe["required_capabilities"] = json!(["mcp_streamable_http", "disposable_workspace"]);
        assert!(validate_value(value).is_err());

        let mut value = bundled_value();
        let probe = value["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "coordinator-message-idempotency-expiry-v1")
            .unwrap();
        probe["required_capabilities"] = json!([
            "mcp_streamable_http",
            "disposable_workspace",
            "preexisting_test_agents"
        ]);
        assert!(validate_value(value).is_err());
    }

    #[test]
    fn native_probe_without_capability_fails_closed() {
        let mut value = bundled_value();
        let probe = value["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-policy-default-off-v1")
            .unwrap();
        probe["required_capabilities"] = json!(["mcp_streamable_http"]);
        assert!(validate_value(value).is_err());
    }

    #[test]
    fn native_permission_probe_requires_live_independent_cases_and_fail_closed_evidence() {
        let manifest = CampaignManifest::bundled().unwrap();
        let permission = manifest
            .probe("native-permission-park-decisions-v1")
            .unwrap();
        assert!(!permission.eligibility.offline);
        for capability in [
            RunnerCapability::LiveProviderRoute,
            RunnerCapability::ProviderObservation,
            RunnerCapability::AuthoritativeUsage,
        ] {
            assert!(permission.required_capabilities.contains(&capability));
        }
        for action in [
            ProbeAction::SubmitIndependentPermissionCases,
            ProbeAction::AuthorizeWorkExecution,
            ProbeAction::AllowPermission,
            ProbeAction::DenyPermission,
        ] {
            assert!(permission.actions.contains(&action));
        }

        let mut offline = bundled_value();
        let probe = offline["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-permission-park-decisions-v1")
            .unwrap();
        probe["eligibility"]["offline"] = json!(true);
        assert!(validate_value(offline).is_err());

        let mut missing_observer = bundled_value();
        let probe = missing_observer["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-permission-park-decisions-v1")
            .unwrap();
        probe["required_capabilities"]
            .as_array_mut()
            .unwrap()
            .retain(|value| value != "provider_observation");
        assert!(validate_value(missing_observer).is_err());

        let mut one_case = bundled_value();
        let probe = one_case["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-permission-park-decisions-v1")
            .unwrap();
        probe["actions"]
            .as_array_mut()
            .unwrap()
            .retain(|value| value != "deny_permission");
        assert!(validate_value(one_case).is_err());

        let mut no_authorization = bundled_value();
        let probe = no_authorization["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-permission-park-decisions-v1")
            .unwrap();
        probe["actions"]
            .as_array_mut()
            .unwrap()
            .retain(|value| value != "authorize_work_execution");
        assert!(validate_value(no_authorization).is_err());
    }

    #[test]
    fn native_retry_restart_and_no_duplicate_probes_reject_internal_or_false_states() {
        let mut retrying = bundled_value();
        let probe = retrying["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-interruption-retry-policy-v1")
            .unwrap();
        probe["actions"]
            .as_array_mut()
            .unwrap()
            .push(json!("interrupt_run"));
        probe["expected_transitions"] = json!([{
            "entity": "work",
            "from": "interrupted",
            "to": "retrying"
        }]);
        assert!(validate_value(retrying).is_err());

        let mut recovered = bundled_value();
        let probe = recovered["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-restart-intent-adoption-v1")
            .unwrap();
        probe["expected_transitions"] = json!([{
            "entity": "execution_intent",
            "from": "active",
            "to": "recovered"
        }]);
        assert!(validate_value(recovered).is_err());

        let mut deduplicated_run = bundled_value();
        let probe = deduplicated_run["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "native-no-duplicate-run-v1")
            .unwrap();
        probe["expected_transitions"] = json!([{
            "entity": "run",
            "from": "created",
            "to": "deduplicated"
        }]);
        assert!(validate_value(deduplicated_run).is_err());
    }

    #[test]
    fn action_tool_and_self_contained_setup_mismatches_fail_closed() {
        let mut missing_tool = bundled_value();
        let probe = missing_tool["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "work-lifecycle-v1")
            .unwrap();
        probe["required_tools"] = json!(["ptah_create_work"]);
        assert!(validate_value(missing_tool).is_err());

        let mut missing_setup = bundled_value();
        let probe = missing_setup["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "core-checkpoint-inspection-v1")
            .unwrap();
        probe["actions"] = json!(["inspect_checkpoint"]);
        assert!(validate_value(missing_setup).is_err());

        let mut missing_routine_agent = bundled_value();
        let probe = missing_routine_agent["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "routine-activation-dedupe-v1")
            .unwrap();
        probe["actions"]
            .as_array_mut()
            .unwrap()
            .retain(|action| action != "create_test_agents");
        assert!(validate_value(missing_routine_agent).is_err());

        let mut conflicting_admission = bundled_value();
        let probe = conflicting_admission["probes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|probe| probe["id"] == "work-lifecycle-v1")
            .unwrap();
        probe["actions"]
            .as_array_mut()
            .unwrap()
            .insert(2, json!("assign_work"));
        probe["required_tools"]
            .as_array_mut()
            .unwrap()
            .push(json!("ptah_assign_work"));
        assert!(validate_value(conflicting_admission).is_err());

        let mut widened = bundled_value();
        widened["campaign_bounds"]["max_total_tokens"] = json!(2_000_001_u64);
        assert!(validate_value(widened).is_err());
    }

    #[test]
    fn malformed_oversized_and_unsafe_catalog_reference_fail_closed() {
        assert!(CampaignManifest::from_slice(b"{").is_err());
        assert!(CampaignManifest::from_slice(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());

        let mut value = bundled_value();
        value["catalog"]["relative_path"] = json!("../../outside.json");
        assert!(CampaignManifest::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn manifest_descriptions_cannot_smuggle_credential_shaped_values() {
        let mut value = bundled_value();
        value["probes"][0]["description"] =
            json!("probe xai-0123456789abcdef0123456789abcdef must not persist");
        assert!(validate_value(value).is_err());
    }

    #[test]
    fn checked_loader_is_root_bound() {
        let root = tempdir().unwrap();
        let lab = root.path().join("evals/certification-lab");
        fs::create_dir_all(&lab).unwrap();
        fs::write(lab.join("campaign.v1.json"), BUNDLED_MANIFEST).unwrap();
        fs::write(
            root.path().join("evals/persistent-agent-scenarios.v1.json"),
            BUNDLED_CATALOG,
        )
        .unwrap();
        CampaignManifest::load_checked(&lab.join("campaign.v1.json"), root.path()).unwrap();

        let outside = tempdir().unwrap();
        assert!(
            CampaignManifest::load_checked(&lab.join("campaign.v1.json"), outside.path()).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn checked_loader_rejects_catalog_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let lab = root.path().join("evals/certification-lab");
        fs::create_dir_all(&lab).unwrap();
        fs::write(lab.join("campaign.v1.json"), BUNDLED_MANIFEST).unwrap();
        let real = root.path().join("real-catalog.json");
        fs::write(&real, BUNDLED_CATALOG).unwrap();
        symlink(
            &real,
            root.path().join("evals/persistent-agent-scenarios.v1.json"),
        )
        .unwrap();
        assert!(
            CampaignManifest::load_checked(&lab.join("campaign.v1.json"), root.path()).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn checked_loader_rejects_a_symlinked_manifest_ancestor() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let real_evals = root.path().join("real-evals");
        let lab = real_evals.join("certification-lab");
        fs::create_dir_all(&lab).unwrap();
        fs::write(lab.join("campaign.v1.json"), BUNDLED_MANIFEST).unwrap();
        fs::write(
            real_evals.join("persistent-agent-scenarios.v1.json"),
            BUNDLED_CATALOG,
        )
        .unwrap();
        symlink(&real_evals, root.path().join("evals")).unwrap();
        assert!(CampaignManifest::load_checked(
            &root.path().join("evals/certification-lab/campaign.v1.json"),
            root.path()
        )
        .is_err());
    }

    #[test]
    fn catalog_duplicate_ids_are_rejected() {
        let manifest = CampaignManifest::from_slice(BUNDLED_MANIFEST).unwrap();
        let mut catalog: Value = serde_json::from_slice(BUNDLED_CATALOG).unwrap();
        let duplicate = catalog["scenarios"][0].clone();
        catalog["scenarios"].as_array_mut().unwrap().push(duplicate);
        assert!(manifest
            .validate_against_catalog_slice(&serde_json::to_vec(&catalog).unwrap())
            .is_err());
    }

    #[test]
    fn catalog_reference_path_is_the_only_parent_component_allowed() {
        let path = std::path::PathBuf::from(AUTHORITATIVE_CATALOG_RELATIVE_PATH);
        assert_eq!(path.components().next(), Some(Component::ParentDir));
        assert!(validate_relative_path(AUTHORITATIVE_CATALOG_RELATIVE_PATH).is_ok());
        assert!(validate_relative_path("/tmp/catalog.json").is_err());
    }
}
