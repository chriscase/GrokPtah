//! Bounded, secret-free campaign report and structural trace schemas.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
use grokptah_agent_bridge::certification::PERSISTENT_AGENT_SCENARIO_IDS;
use grokptah_agent_bridge::{
    public_xai_endpoint_fingerprint, scan_value_for_forbidden_data, CampaignActuals,
    CredentialMethodClass, PersistentAgentCapture, ProviderRouteClass, MAX_CAPTURE_BYTES,
    PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{
    CampaignManifest, DurableEntity, DurableState, OracleCode, ProbeAction, ProbeDefinition,
    ProbeScope, MAX_MANIFEST_BYTES,
};
use crate::{LAB_REPORT_SCHEMA, LAB_TRACE_SCHEMA};

pub const CATALOG_SCHEMA: &str = "grokptah.persistent_agent_scenarios.v1";
pub const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TRACE_BYTES: usize = 512 * 1024;
pub const MAX_TRACE_RECORDS: usize = 4_096;
pub const MAX_PROBES: usize = 256;
pub const MAX_PHASES_PER_PROBE: usize = 64;
pub const MAX_TRANSITIONS_PER_PROBE: usize = 256;
pub const MAX_OPAQUE_IDS_PER_PROBE: usize = 64;
pub const MAX_COUNTER_VALUE: u64 = 10_000_000;
pub const MAX_SINGLE_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_CAMPAIGN_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_PHASE_MILLIS: u64 = 24 * 60 * 60 * 1_000;
const MAX_ARTIFACT_PATH_BYTES: usize = 1_024;
const MAX_ARTIFACT_PATH_COMPONENTS: usize = 64;
const MAX_ARTIFACT_PATH_COMPONENT_BYTES: usize = 255;
const MAX_DISCOVERED_TOOLS: u32 = 512;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Offline,
    Live,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportMode {
    LocalLaunch,
    Attach,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Passed,
    Failed,
    Skipped,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseCode {
    Preflight,
    CapabilityDiscovery,
    Setup,
    Mutate,
    Observe,
    Disconnect,
    Reconnect,
    Restart,
    Oracle,
    Capture,
    Cleanup,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    None,
    Capability,
    Configuration,
    Authentication,
    Transport,
    Protocol,
    DurableState,
    Oracle,
    Bounds,
    Safety,
    Provider,
    Interrupted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    Ok,
    CapabilityAbsent,
    ProbeImplementationUnavailable,
    ModeIneligible,
    TransportIneligible,
    AttachMutationOptInRequired,
    DeterministicClockUnavailable,
    ManagedExecutionUnavailable,
    PermissionCapabilityAbsent,
    ProviderObservationUnavailable,
    ProviderObservationDropped,
    AuthoritativeUsageMissing,
    LiveOptInMissing,
    AuthenticationUnavailable,
    ServiceUnreachable,
    ServiceNotReady,
    ToolDiscoveryFailed,
    RequiredToolMissing,
    McpInitializeFailed,
    McpCallFailed,
    McpToolError,
    McpResultMalformed,
    ScopeRejected,
    StateTransitionMismatch,
    TerminalStateMissing,
    CursorContinuityLost,
    RestartControlUnavailable,
    RestartRecoveryFailed,
    IdempotencyReplayMismatch,
    IdempotencyConflictObserved,
    IdempotencyConflictAccepted,
    IdempotencyConflictUnproven,
    BoundExceeded,
    Timeout,
    Interrupted,
    RedactionRejected,
    ArtifactBudgetExceeded,
    OutputPathUnsafe,
    CaptureInvalid,
    FixtureInvalid,
    OracleMismatch,
    CleanupFailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DurableStateCode {
    Unknown,
    Absent,
    Starting,
    Ready,
    Created,
    Active,
    Waiting,
    Queued,
    Blocked,
    Offered,
    Accepted,
    Claimed,
    Leased,
    Admitted,
    Running,
    AwaitingApproval,
    Parked,
    Allowed,
    Denied,
    Retrying,
    Recovered,
    Finalized,
    Advanced,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Completed,
    LimitReached,
    Enabled,
    Paused,
    Disabled,
    Deduplicated,
    Expired,
    Acknowledged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DurableIdKind {
    Agent,
    Session,
    Run,
    Checkpoint,
    Work,
    Attempt,
    Routine,
    Activation,
    Message,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpaqueDurableId {
    pub kind: DurableIdKind,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransitionEvidence {
    pub entity: EntityKind,
    pub from: DurableStateCode,
    pub to: DurableStateCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opaque_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PhaseResult {
    pub phase: PhaseCode,
    pub status: ProbeStatus,
    pub elapsed_millis: u64,
    pub diagnostics: Vec<DiagnosticCode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCounters {
    pub events: u64,
    pub tool_calls: u64,
    pub permission_requests: u64,
    pub errors: u64,
    pub provider_attempts: u64,
    pub reconnects: u64,
    pub restarts: u64,
    pub dropped_records: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconnectEvidence {
    pub attempted: bool,
    pub reinitialized: bool,
    pub cursor_before: Option<u64>,
    pub cursor_after: Option<u64>,
    pub continuity_proven: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestartEvidence {
    pub attempted: bool,
    pub host_owned: bool,
    pub durable_read_recovered: bool,
    pub event_cursor_recovered: bool,
    pub implicit_execution_observed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureReference {
    pub catalog_scenario_id: String,
    pub artifact: ArtifactReference,
    pub capture_schema: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub probe_id: String,
    pub catalog_scenario_ids: Vec<String>,
    pub status: ProbeStatus,
    pub supported: bool,
    pub failure_class: FailureClass,
    pub diagnostics: Vec<DiagnosticCode>,
    pub verified_actions: Vec<ProbeAction>,
    pub verified_oracles: Vec<OracleCode>,
    pub phases: Vec<PhaseResult>,
    pub transitions: Vec<TransitionEvidence>,
    pub counters: EvidenceCounters,
    pub reconnect: ReconnectEvidence,
    pub restart: RestartEvidence,
    pub opaque_ids: Vec<OpaqueDurableId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<ArtifactReference>,
    pub capture_refs: Vec<CaptureReference>,
    pub elapsed_millis: u64,
}

impl ProbeResult {
    pub fn skipped(
        probe_id: impl Into<String>,
        catalog_scenario_ids: Vec<String>,
        diagnostic: DiagnosticCode,
    ) -> Self {
        Self {
            probe_id: probe_id.into(),
            catalog_scenario_ids,
            status: ProbeStatus::Skipped,
            supported: false,
            failure_class: diagnostic_failure_class(ProbeStatus::Skipped, diagnostic),
            diagnostics: vec![diagnostic],
            verified_actions: Vec::new(),
            verified_oracles: Vec::new(),
            phases: vec![PhaseResult {
                phase: PhaseCode::CapabilityDiscovery,
                status: ProbeStatus::Skipped,
                elapsed_millis: 0,
                diagnostics: vec![diagnostic],
            }],
            transitions: Vec::new(),
            counters: EvidenceCounters::default(),
            reconnect: ReconnectEvidence::default(),
            restart: RestartEvidence::default(),
            opaque_ids: Vec::new(),
            trace: None,
            capture_refs: Vec::new(),
            elapsed_millis: 0,
        }
    }

    pub fn indeterminate(
        probe_id: impl Into<String>,
        catalog_scenario_ids: Vec<String>,
        diagnostic: DiagnosticCode,
    ) -> Self {
        let mut result = Self::skipped(probe_id, catalog_scenario_ids, diagnostic);
        result.status = ProbeStatus::Indeterminate;
        result.supported = true;
        result.failure_class = diagnostic_failure_class(ProbeStatus::Indeterminate, diagnostic);
        result.phases[0].status = ProbeStatus::Indeterminate;
        result
    }
}

pub fn diagnostic_failure_class(status: ProbeStatus, diagnostic: DiagnosticCode) -> FailureClass {
    if status == ProbeStatus::Passed {
        return FailureClass::None;
    }
    match diagnostic {
        DiagnosticCode::CapabilityAbsent
        | DiagnosticCode::RequiredToolMissing
        | DiagnosticCode::ManagedExecutionUnavailable
        | DiagnosticCode::PermissionCapabilityAbsent
        | DiagnosticCode::DeterministicClockUnavailable => FailureClass::Capability,
        DiagnosticCode::ProbeImplementationUnavailable
        | DiagnosticCode::ModeIneligible
        | DiagnosticCode::TransportIneligible
        | DiagnosticCode::AttachMutationOptInRequired
        | DiagnosticCode::LiveOptInMissing => FailureClass::Configuration,
        DiagnosticCode::AuthenticationUnavailable => FailureClass::Authentication,
        DiagnosticCode::ServiceUnreachable
        | DiagnosticCode::McpInitializeFailed
        | DiagnosticCode::McpCallFailed => FailureClass::Transport,
        DiagnosticCode::ToolDiscoveryFailed
        | DiagnosticCode::McpToolError
        | DiagnosticCode::McpResultMalformed
        | DiagnosticCode::IdempotencyReplayMismatch
        | DiagnosticCode::IdempotencyConflictObserved
        | DiagnosticCode::IdempotencyConflictAccepted
        | DiagnosticCode::IdempotencyConflictUnproven => FailureClass::Protocol,
        DiagnosticCode::StateTransitionMismatch
        | DiagnosticCode::TerminalStateMissing
        | DiagnosticCode::CursorContinuityLost
        | DiagnosticCode::RestartRecoveryFailed => FailureClass::DurableState,
        DiagnosticCode::BoundExceeded
        | DiagnosticCode::Timeout
        | DiagnosticCode::ArtifactBudgetExceeded => FailureClass::Bounds,
        DiagnosticCode::RedactionRejected
        | DiagnosticCode::OutputPathUnsafe
        | DiagnosticCode::ScopeRejected => FailureClass::Safety,
        DiagnosticCode::ProviderObservationUnavailable
        | DiagnosticCode::ProviderObservationDropped
        | DiagnosticCode::AuthoritativeUsageMissing => FailureClass::Provider,
        DiagnosticCode::Interrupted => FailureClass::Interrupted,
        DiagnosticCode::Ok
        | DiagnosticCode::ServiceNotReady
        | DiagnosticCode::RestartControlUnavailable
        | DiagnosticCode::CaptureInvalid
        | DiagnosticCode::FixtureInvalid
        | DiagnosticCode::OracleMismatch
        | DiagnosticCode::CleanupFailed => FailureClass::Oracle,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportSummary {
    pub probes: u32,
    pub supported: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub indeterminate: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRouteCode {
    GrokBuildProxy,
    XaiApi,
    CompatibleGateway,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMethodCode {
    GrokBuildOidc,
    ApiKeyReference,
    ManagedProviderReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderSummary {
    pub route_class: ProviderRouteCode,
    pub model_identity: String,
    pub credential_method: CredentialMethodCode,
    pub endpoint_fingerprint: String,
    pub observation_complete: bool,
    pub authoritative_usage_complete: bool,
    pub observation_dropped_records: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionPolicy {
    PositiveSchemaV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedactionMetadata {
    pub policy: RedactionPolicy,
    pub fields_omitted: u64,
    pub strings_truncated: u64,
    pub reasoning_records_dropped: u64,
    pub binary_records_dropped: u64,
    pub forbidden_scan_passed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TruncationMetadata {
    pub traces_truncated: u32,
    pub records_dropped: u64,
    pub bytes_written: u64,
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignReport {
    pub schema: String,
    pub campaign_id: String,
    pub run_id: String,
    pub manifest_sha256: String,
    pub manifest_artifact: ArtifactReference,
    pub selected_probe_ids: Vec<String>,
    pub catalog_schema: String,
    pub repository_commit: String,
    pub repository_dirty: bool,
    pub started_at: String,
    pub finished_at: String,
    pub runtime_mode: RuntimeMode,
    pub transport_mode: TransportMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_actuals: Option<CampaignActuals>,
    pub discovered_tool_count: u32,
    pub summary: ReportSummary,
    pub counters: EvidenceCounters,
    pub probes: Vec<ProbeResult>,
    pub certified: bool,
    pub redaction: RedactionMetadata,
    pub truncation: TruncationMetadata,
}

impl CampaignReport {
    pub fn recompute_summary(&mut self) -> Result<()> {
        self.summary = ReportSummary {
            probes: u32::try_from(self.probes.len()).context("probe count overflow")?,
            supported: u32::try_from(self.probes.iter().filter(|probe| probe.supported).count())
                .context("supported count overflow")?,
            passed: count_status(&self.probes, ProbeStatus::Passed)?,
            failed: count_status(&self.probes, ProbeStatus::Failed)?,
            skipped: count_status(&self.probes, ProbeStatus::Skipped)?,
            indeterminate: count_status(&self.probes, ProbeStatus::Indeterminate)?,
        };
        self.counters = self
            .probes
            .iter()
            .try_fold(EvidenceCounters::default(), |total, probe| {
                checked_add_counters(&total, &probe.counters)
            })?;
        if let Some(provider) = &self.provider {
            self.counters.dropped_records = self
                .counters
                .dropped_records
                .checked_add(provider.observation_dropped_records)
                .context("provider observation drop counter overflow")?;
            validate_counter(self.counters.dropped_records)?;
        }
        Ok(())
    }

    /// Validate the report and every referenced trace/capture relative to the
    /// exact campaign root. A boolean inside the report is never treated as
    /// evidence that a capture was validated.
    pub fn validate_at(&self, campaign_root: &Path) -> Result<Vec<u8>> {
        let report_bytes = self.validate_structure()?;
        let root = canonical_regular_directory(campaign_root)?;
        let mut artifact_paths = HashSet::new();
        let mut referenced_bytes = 0_u64;
        validate_artifact_reference(&self.manifest_artifact)?;
        if self.manifest_artifact.bytes > MAX_MANIFEST_BYTES as u64
            || !artifact_paths.insert(self.manifest_artifact.relative_path.as_str())
        {
            bail!("report manifest artifact is duplicated or oversized");
        }
        let manifest_bytes =
            verify_artifact(&root, &self.manifest_artifact, MAX_MANIFEST_BYTES as u64)?;
        if self.manifest_artifact.sha256 != self.manifest_sha256 {
            bail!("report manifest digest does not match its artifact");
        }
        let manifest = CampaignManifest::from_slice(&manifest_bytes)
            .map_err(|_| anyhow::anyhow!("referenced campaign manifest is invalid"))?;
        let selected = manifest
            .select_probes(&self.selected_probe_ids)
            .map_err(|_| anyhow::anyhow!("report selected probe contract is invalid"))?;
        if selected.len() != self.probes.len()
            || selected
                .iter()
                .zip(&self.probes)
                .any(|(definition, result)| definition.id != result.probe_id)
        {
            bail!("report omits, adds, or reorders a selected manifest probe");
        }
        if self.truncation.artifact_budget_bytes > manifest.campaign_bounds.max_artifact_bytes {
            bail!("report artifact budget exceeds the authoritative campaign profile");
        }
        let elapsed_seconds = parse_timestamp(&self.finished_at)?
            .signed_duration_since(parse_timestamp(&self.started_at)?)
            .num_seconds();
        if elapsed_seconds < 0
            || u64::try_from(elapsed_seconds).context("campaign elapsed duration conversion")?
                > manifest.campaign_bounds.max_duration_seconds
        {
            bail!("campaign elapsed duration exceeds the selected manifest bound");
        }
        referenced_bytes = referenced_bytes
            .checked_add(self.manifest_artifact.bytes)
            .context("referenced artifact byte total overflow")?;

        let mut trace_truncations = 0_u32;
        let mut trace_dropped_records = 0_u64;
        let mut provider_actuals = CampaignActuals {
            total_tokens: 0,
            provider_requests: 0,
            continuations: 0,
            duration_seconds: 0,
            raw_artifact_bytes: 0,
        };
        let mut capture_count = 0_u32;
        for (definition, probe) in selected.into_iter().zip(&self.probes) {
            validate_probe_against_definition(probe, definition)?;
            if let Some(trace_ref) = &probe.trace {
                if !artifact_paths.insert(trace_ref.relative_path.as_str()) {
                    bail!("report references an artifact path more than once");
                }
                let bytes = verify_artifact(&root, trace_ref, MAX_TRACE_BYTES as u64)?;
                let trace: StructuralTrace = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("referenced trace is malformed"))?;
                trace.validate()?;
                if trace.probe_id != probe.probe_id {
                    bail!("referenced trace belongs to a different probe");
                }
                if probe.counters.dropped_records != trace.dropped_records {
                    bail!("probe drop counter does not match its verified trace");
                }
                if trace.truncated {
                    trace_truncations = trace_truncations
                        .checked_add(1)
                        .context("trace truncation count overflow")?;
                }
                trace_dropped_records = trace_dropped_records
                    .checked_add(trace.dropped_records)
                    .context("trace dropped-record count overflow")?;
                referenced_bytes = referenced_bytes
                    .checked_add(trace_ref.bytes)
                    .context("referenced artifact byte total overflow")?;
            }
            let mut probe_capture_attempts = 0_u64;
            for capture_ref in &probe.capture_refs {
                if !artifact_paths.insert(capture_ref.artifact.relative_path.as_str()) {
                    bail!("report references an artifact path more than once");
                }
                let bytes =
                    verify_artifact(&root, &capture_ref.artifact, MAX_CAPTURE_BYTES as u64)?;
                let capture: PersistentAgentCapture = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("referenced capture is malformed"))?;
                capture
                    .validate()
                    .map_err(|_| anyhow::anyhow!("referenced capture is invalid"))?;
                if capture.attempts.iter().any(|attempt| {
                    attempt.request_body.is_some() || attempt.response_body.is_some()
                }) {
                    bail!("structural lab reports reject payload-bearing captures");
                }
                if capture.schema != capture_ref.capture_schema
                    || capture_ref.capture_schema != PERSISTENT_AGENT_CAPTURE_SCHEMA
                    || capture.campaign.scenario_id != capture_ref.catalog_scenario_id
                    || !probe
                        .catalog_scenario_ids
                        .contains(&capture_ref.catalog_scenario_id)
                {
                    bail!("capture reference does not match its validated capture");
                }
                validate_capture_binding(self, definition, probe, &capture)?;
                if probe.status == ProbeStatus::Passed
                    && definition.scope == ProbeScope::ProviderStructural
                {
                    capture
                        .validate_complete_structural_evidence()
                        .map_err(|_| anyhow::anyhow!("passing provider capture is incomplete"))?;
                }
                add_capture_actuals(&mut provider_actuals, &capture.actuals)?;
                probe_capture_attempts = probe_capture_attempts
                    .checked_add(u64::from(capture.actuals.provider_requests))
                    .context("probe provider-attempt total overflow")?;
                capture_count = capture_count
                    .checked_add(1)
                    .context("capture count overflow")?;
                referenced_bytes = referenced_bytes
                    .checked_add(capture_ref.artifact.bytes)
                    .context("referenced artifact byte total overflow")?;
            }
            if !probe.capture_refs.is_empty()
                && probe.counters.provider_attempts != probe_capture_attempts
            {
                bail!("probe provider-attempt counter does not match validated captures");
            }
            if probe.status == ProbeStatus::Passed
                && definition.scope == ProbeScope::ProviderStructural
                && (probe.capture_refs.is_empty() || probe.counters.provider_attempts == 0)
            {
                bail!("passing provider probe has no validated provider evidence");
            }
        }
        if trace_truncations != self.truncation.traces_truncated
            || trace_dropped_records != self.truncation.records_dropped
        {
            bail!("report truncation metadata does not match verified traces");
        }
        let observer_drops = self
            .provider
            .as_ref()
            .map_or(0, |provider| provider.observation_dropped_records);
        let expected_drops = trace_dropped_records
            .checked_add(observer_drops)
            .context("aggregate dropped-record count overflow")?;
        if self.counters.dropped_records != expected_drops {
            bail!("report dropped-record counter does not match verified evidence");
        }
        match (capture_count, &self.provider_actuals) {
            (0, None) => {}
            (0, Some(_)) => bail!("report has provider actuals without captures"),
            (_, Some(actuals)) if *actuals == provider_actuals => {}
            _ => bail!("report provider actuals do not match validated captures"),
        }
        if capture_count > 0
            && (provider_actuals.total_tokens > manifest.campaign_bounds.max_total_tokens
                || provider_actuals.provider_requests
                    > manifest.campaign_bounds.max_provider_requests
                || provider_actuals.continuations > manifest.campaign_bounds.max_continuations
                || provider_actuals.duration_seconds
                    > manifest.campaign_bounds.max_duration_seconds
                || provider_actuals.raw_artifact_bytes
                    > manifest.campaign_bounds.max_artifact_bytes)
        {
            bail!("provider actuals exceed the selected campaign bounds");
        }
        let referenced_with_report = referenced_bytes
            .checked_add(u64::try_from(report_bytes.len()).context("report byte conversion")?)
            .context("campaign report artifact byte total overflow")?;
        if referenced_with_report > self.truncation.artifact_budget_bytes
            || self.truncation.bytes_written < referenced_bytes
        {
            bail!("referenced artifacts exceed the campaign artifact budget");
        }
        Ok(report_bytes)
    }

    fn validate_structure(&self) -> Result<Vec<u8>> {
        if self.schema != LAB_REPORT_SCHEMA {
            bail!("unsupported campaign report schema");
        }
        validate_safe_id(&self.campaign_id).context("campaign_id")?;
        validate_safe_id(&self.run_id).context("run_id")?;
        validate_hash(&self.manifest_sha256).context("manifest hash")?;
        validate_artifact_reference(&self.manifest_artifact)?;
        if self.manifest_artifact.sha256 != self.manifest_sha256 {
            bail!("manifest artifact digest differs from the report binding");
        }
        if self.catalog_schema != CATALOG_SCHEMA {
            bail!("unsupported catalog schema in report");
        }
        validate_commit(&self.repository_commit)?;
        let started = parse_timestamp(&self.started_at)?;
        let finished = parse_timestamp(&self.finished_at)?;
        if finished < started {
            bail!("campaign finished before it started");
        }
        if self.probes.is_empty()
            || self.probes.len() > MAX_PROBES
            || self.selected_probe_ids.is_empty()
            || self.selected_probe_ids.len() > MAX_PROBES
        {
            bail!("campaign report exceeds the probe bound");
        }
        if self.discovered_tool_count == 0 || self.discovered_tool_count > MAX_DISCOVERED_TOOLS {
            bail!("discovered MCP tool count is outside its bound");
        }
        if self.truncation.artifact_budget_bytes == 0
            || self.truncation.artifact_budget_bytes > MAX_CAMPAIGN_ARTIFACT_BYTES
            || self.truncation.bytes_written > self.truncation.artifact_budget_bytes
            || !self.redaction.forbidden_scan_passed
        {
            bail!("campaign safety metadata is inconsistent");
        }
        validate_counter(self.redaction.fields_omitted)?;
        validate_counter(self.redaction.strings_truncated)?;
        validate_counter(self.redaction.reasoning_records_dropped)?;
        validate_counter(self.redaction.binary_records_dropped)?;
        validate_counter(self.truncation.records_dropped)?;
        if let Some(provider) = &self.provider {
            validate_provider(provider)?;
        }
        if self.provider.is_none() != self.provider_actuals.is_none() {
            bail!("provider identity and actuals must be present together");
        }
        if self.runtime_mode == RuntimeMode::Offline && self.provider.is_some() {
            bail!("offline reports must not fabricate a provider identity");
        }
        let mut selected_ids = HashSet::new();
        for id in &self.selected_probe_ids {
            validate_safe_id(id)?;
            if !selected_ids.insert(id.as_str()) {
                bail!("report repeats a selected probe id");
            }
        }
        let mut probe_ids = HashSet::new();
        for probe in &self.probes {
            if !probe_ids.insert(probe.probe_id.as_str()) {
                bail!("duplicate probe id in report");
            }
            validate_probe(probe)?;
        }
        if selected_ids != probe_ids {
            bail!("report result set differs from its selected probe set");
        }
        validate_counters(&self.counters)?;
        let mut expected = self.clone();
        expected.recompute_summary()?;
        if self.summary != expected.summary || self.counters != expected.counters {
            bail!("campaign report summary does not match probe results");
        }
        let provider_certifiable = match (&self.provider, &self.provider_actuals) {
            (None, None) => true,
            (Some(provider), Some(actuals)) => {
                provider.observation_complete
                    && provider.authoritative_usage_complete
                    && provider.observation_dropped_records == 0
                    && actuals.provider_requests > 0
            }
            _ => false,
        };
        let expected_certified = self.summary.probes > 0
            && !self.repository_dirty
            && self.summary.passed == self.summary.probes
            && self.summary.failed == 0
            && self.summary.skipped == 0
            && self.summary.indeterminate == 0
            && provider_certifiable;
        if self.certified != expected_certified {
            bail!("campaign certification flag is inconsistent with exact probe results");
        }
        let value = serde_json::to_value(self)?;
        scan_value_for_forbidden_data(&value)
            .map_err(|_| anyhow::anyhow!("campaign report failed forbidden-data scanning"))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > MAX_REPORT_BYTES {
            bail!("campaign report exceeds the serialized byte bound");
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceOperationCode {
    Initialize,
    ListTools,
    GetCapacity,
    CreateSession,
    ListSessions,
    ListAgents,
    GetAgent,
    SubmitRun,
    GetRun,
    ListRuns,
    GetEvents,
    SteerRun,
    CancelRun,
    ResumeAgent,
    CreateWork,
    ListWork,
    GetWork,
    AssignWork,
    ClaimWork,
    RenewWork,
    ProgressWork,
    CompleteWork,
    FailWork,
    CancelWork,
    ApproveWork,
    RetryWork,
    ReleaseWork,
    CreateRoutine,
    FireRoutine,
    SetRoutineLifecycle,
    ListActivations,
    HeartbeatWorker,
    OfferWork,
    AcceptWork,
    DeclineWork,
    ReassignWork,
    ReprioritizeWork,
    BlockWork,
    RequestReview,
    ListDecisions,
    SendMessage,
    ListMessages,
    ListInbox,
    AckMessage,
    Disconnect,
    Reconnect,
    Restart,
    GetManagedExecution,
    SetManagedExecution,
    AuthorizeWorkExecution,
    ResolveWorkInput,
    ListExecutionIntents,
    CreateManagerPlan,
    TickManagerPlan,
    GetManagerPlan,
    Oracle,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentFieldCode {
    RequestId,
    SessionId,
    Workspace,
    AgentId,
    RunId,
    MaxRounds,
    WorkId,
    AttemptId,
    RoutineId,
    MessageId,
    Bounds,
    ExecutionMode,
    AllowQueue,
    Kind,
    Objective,
    Prompt,
    Policy,
    Dependencies,
    LeaseMs,
    Percent,
    Trigger,
    WorkTemplate,
    ExpectedRevision,
    AfterSequence,
    Limit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceRecord {
    pub ordinal: u32,
    pub operation: TraceOperationCode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argument_fields: Vec<ArgumentFieldCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<DiagnosticCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuralTrace {
    pub schema: String,
    pub probe_id: String,
    pub records: Vec<TraceRecord>,
    pub truncated: bool,
    pub dropped_records: u64,
}

impl StructuralTrace {
    pub fn validate(&self) -> Result<Vec<u8>> {
        if self.schema != LAB_TRACE_SCHEMA {
            bail!("unsupported structural trace schema");
        }
        validate_safe_id(&self.probe_id)?;
        if self.records.len() > MAX_TRACE_RECORDS {
            bail!("structural trace exceeds the record bound");
        }
        validate_counter(self.dropped_records)?;
        if self.truncated != (self.dropped_records > 0) {
            bail!("structural trace truncation metadata is inconsistent");
        }
        let mut last_sequence = None;
        for (index, record) in self.records.iter().enumerate() {
            if record.ordinal as usize != index + 1 {
                bail!("structural trace ordinals are not contiguous");
            }
            let unique: HashSet<_> = record.argument_fields.iter().collect();
            if unique.len() != record.argument_fields.len() {
                bail!("structural trace repeats an argument field");
            }
            if let Some(sequence) = record.sequence {
                if sequence == 0 || last_sequence.is_some_and(|prior| sequence < prior) {
                    bail!("structural trace sequence is zero or regresses");
                }
                last_sequence = Some(sequence);
            }
        }
        let value = serde_json::to_value(self)?;
        scan_value_for_forbidden_data(&value)
            .map_err(|_| anyhow::anyhow!("structural trace failed forbidden-data scanning"))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > MAX_TRACE_BYTES {
            bail!("structural trace exceeds the serialized byte bound");
        }
        Ok(bytes)
    }
}

pub fn opaque_durable_id(value: &str) -> String {
    format!("opaque-{:x}", Sha256::digest(value.as_bytes()))
}

pub fn validate_artifact_reference(reference: &ArtifactReference) -> Result<()> {
    let path = Path::new(&reference.relative_path);
    if reference.relative_path.is_empty()
        || reference.relative_path.len() > MAX_ARTIFACT_PATH_BYTES
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("artifact reference is not a safe relative path");
    }
    let components: Vec<_> = path.components().collect();
    if components.is_empty()
        || components.len() > MAX_ARTIFACT_PATH_COMPONENTS
        || components.iter().any(|component| {
            component.as_os_str().to_string_lossy().len() > MAX_ARTIFACT_PATH_COMPONENT_BYTES
        })
    {
        bail!("artifact reference component bounds are invalid");
    }
    validate_hash(&reference.sha256)?;
    if reference.bytes == 0 || reference.bytes > MAX_SINGLE_ARTIFACT_BYTES {
        bail!("artifact reference byte length is outside its bound");
    }
    Ok(())
}

pub fn validate_safe_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("invalid stable identifier");
    }
    Ok(())
}

fn validate_probe(probe: &ProbeResult) -> Result<()> {
    validate_safe_id(&probe.probe_id)?;
    if probe.phases.is_empty()
        || probe.phases.len() > MAX_PHASES_PER_PROBE
        || probe.transitions.len() > MAX_TRANSITIONS_PER_PROBE
        || probe.opaque_ids.len() > MAX_OPAQUE_IDS_PER_PROBE
        || probe.elapsed_millis > MAX_PHASE_MILLIS
    {
        bail!("probe evidence exceeds a structural bound");
    }
    if probe.catalog_scenario_ids.is_empty() {
        bail!("probe must reference at least one authoritative catalog scenario");
    }
    if probe.verified_actions.len() > 64 || probe.verified_oracles.len() > 64 {
        bail!("probe verified-contract evidence exceeds its bound");
    }
    if probe.verified_actions.iter().collect::<HashSet<_>>().len() != probe.verified_actions.len()
        || probe.verified_oracles.iter().collect::<HashSet<_>>().len()
            != probe.verified_oracles.len()
    {
        bail!("probe repeats a verified action or oracle");
    }
    match probe.status {
        ProbeStatus::Skipped if probe.supported => bail!("skipped probe is marked supported"),
        ProbeStatus::Passed | ProbeStatus::Failed | ProbeStatus::Indeterminate
            if !probe.supported =>
        {
            bail!("non-skipped probe is marked unsupported")
        }
        _ => {}
    }
    if (probe.status == ProbeStatus::Passed) != (probe.failure_class == FailureClass::None) {
        bail!("probe failure class is inconsistent with its status");
    }
    validate_diagnostics(&probe.diagnostics, probe.status)?;
    if probe.failure_class != diagnostic_failure_class(probe.status, probe.diagnostics[0]) {
        bail!("probe failure class does not match its typed diagnostic");
    }
    if probe.phases.last().map(|phase| phase.status) != Some(probe.status) {
        bail!("probe terminal phase status does not match probe status");
    }
    for phase in &probe.phases {
        if phase.elapsed_millis > MAX_PHASE_MILLIS {
            bail!("probe phase exceeds its duration bound");
        }
        validate_diagnostics(&phase.diagnostics, phase.status)?;
    }
    if probe.status == ProbeStatus::Passed
        && probe
            .phases
            .iter()
            .any(|phase| phase.status != ProbeStatus::Passed)
    {
        bail!("passing probe contains a non-passing phase");
    }
    let mut catalog_ids = HashSet::new();
    for scenario_id in &probe.catalog_scenario_ids {
        validate_catalog_scenario_id(scenario_id)?;
        if !catalog_ids.insert(scenario_id.as_str()) {
            bail!("probe repeats a catalog scenario id");
        }
    }
    for transition in &probe.transitions {
        if let Some(value) = &transition.opaque_id {
            validate_opaque_id(value)?;
        }
    }
    let mut durable_ids = HashSet::new();
    for durable_id in &probe.opaque_ids {
        validate_opaque_id(&durable_id.value)?;
        if !durable_ids.insert((durable_id.kind, durable_id.value.as_str())) {
            bail!("probe repeats an opaque durable id");
        }
    }
    validate_counters(&probe.counters)?;
    if !probe.reconnect.attempted
        && (probe.reconnect.reinitialized
            || probe.reconnect.cursor_before.is_some()
            || probe.reconnect.cursor_after.is_some()
            || probe.reconnect.continuity_proven)
    {
        bail!("reconnect evidence is populated without an attempt");
    }
    if probe.reconnect.continuity_proven {
        let (Some(before), Some(after)) =
            (probe.reconnect.cursor_before, probe.reconnect.cursor_after)
        else {
            bail!("reconnect continuity has no concrete cursor pair");
        };
        if !probe.reconnect.attempted || !probe.reconnect.reinitialized || after < before {
            bail!("reconnect evidence claims continuity without proof");
        }
    }
    if !probe.restart.attempted
        && (probe.restart.host_owned
            || probe.restart.durable_read_recovered
            || probe.restart.event_cursor_recovered
            || probe.restart.implicit_execution_observed)
    {
        bail!("restart evidence is populated without an attempt");
    }
    if probe.restart.durable_read_recovered
        && (!probe.restart.attempted || !probe.restart.host_owned)
    {
        bail!("restart evidence claims recovery without owned restart control");
    }
    if let Some(trace) = &probe.trace {
        validate_artifact_reference(trace)?;
        if trace.bytes > MAX_TRACE_BYTES as u64 {
            bail!("trace reference exceeds the trace byte bound");
        }
    }
    let mut capture_paths = HashSet::new();
    for capture in &probe.capture_refs {
        validate_catalog_scenario_id(&capture.catalog_scenario_id)?;
        if capture.capture_schema != PERSISTENT_AGENT_CAPTURE_SCHEMA {
            bail!("capture reference uses an unsupported capture schema");
        }
        validate_artifact_reference(&capture.artifact)?;
        if capture.artifact.bytes > MAX_CAPTURE_BYTES as u64
            || !capture_paths.insert(capture.artifact.relative_path.as_str())
        {
            bail!("capture reference is duplicated or oversized");
        }
    }
    Ok(())
}

fn validate_probe_against_definition(
    probe: &ProbeResult,
    definition: &ProbeDefinition,
) -> Result<()> {
    if probe.catalog_scenario_ids != definition.catalog_scenario_ids {
        bail!("probe catalog binding differs from the selected manifest contract");
    }
    let max_millis = definition
        .bounds
        .max_duration_seconds
        .checked_mul(1_000)
        .context("probe duration bound overflow")?;
    if probe.elapsed_millis > max_millis
        || probe
            .phases
            .iter()
            .any(|phase| phase.elapsed_millis > max_millis)
    {
        bail!("probe evidence exceeds its selected manifest duration bound");
    }
    if probe.status == ProbeStatus::Passed {
        if probe.trace.is_none()
            || probe.verified_actions != definition.actions
            || probe.verified_oracles != definition.oracle_codes
        {
            bail!("passing probe lacks its exact action/oracle evidence contract");
        }
        for expected in &definition.expected_transitions {
            let entity = map_entity(expected.entity);
            let from = map_state(expected.from);
            let to = map_state(expected.to);
            if !probe.transitions.iter().any(|observed| {
                observed.entity == entity && observed.from == from && observed.to == to
            }) {
                bail!("passing probe lacks a required durable transition");
            }
        }
    } else if !probe.verified_actions.is_empty() || !probe.verified_oracles.is_empty() {
        bail!("non-passing probe claims completed action/oracle verification");
    }
    if definition.actions.contains(&ProbeAction::RestartService)
        && probe.status == ProbeStatus::Passed
        && (!probe.restart.attempted
            || !probe.restart.host_owned
            || !probe.restart.durable_read_recovered
            || probe.restart.implicit_execution_observed)
    {
        bail!("passing restart probe lacks recovered state or observed implicit execution");
    }
    if definition.scope == ProbeScope::ProviderStructural
        && probe.status == ProbeStatus::Passed
        && probe.capture_refs.is_empty()
    {
        bail!("passing provider probe has no capture");
    }
    if definition.scope == ProbeScope::NativeManagedFuture
        && !definition.eligibility.offline
        && probe.status == ProbeStatus::Passed
        && probe.capture_refs.is_empty()
    {
        bail!("passing live native probe has no provider capture");
    }
    Ok(())
}

fn map_entity(value: DurableEntity) -> EntityKind {
    match value {
        DurableEntity::Campaign => EntityKind::Campaign,
        DurableEntity::Service => EntityKind::Service,
        DurableEntity::Agent => EntityKind::Agent,
        DurableEntity::Run => EntityKind::Run,
        DurableEntity::Work => EntityKind::Work,
        DurableEntity::Attempt => EntityKind::Attempt,
        DurableEntity::Lease => EntityKind::Lease,
        DurableEntity::Routine => EntityKind::Routine,
        DurableEntity::Activation => EntityKind::Activation,
        DurableEntity::Worker => EntityKind::Worker,
        DurableEntity::Message => EntityKind::Message,
        DurableEntity::Cursor => EntityKind::Cursor,
        DurableEntity::Permission => EntityKind::Permission,
        DurableEntity::ExecutionIntent => EntityKind::ExecutionIntent,
    }
}

fn map_state(value: DurableState) -> DurableStateCode {
    match value {
        DurableState::Absent => DurableStateCode::Absent,
        DurableState::Starting => DurableStateCode::Starting,
        DurableState::Ready => DurableStateCode::Ready,
        DurableState::Created => DurableStateCode::Created,
        DurableState::Queued => DurableStateCode::Queued,
        DurableState::Offered => DurableStateCode::Offered,
        DurableState::Accepted => DurableStateCode::Accepted,
        DurableState::Claimed => DurableStateCode::Claimed,
        DurableState::Leased => DurableStateCode::Leased,
        DurableState::Admitted => DurableStateCode::Admitted,
        DurableState::Active => DurableStateCode::Active,
        DurableState::Running => DurableStateCode::Running,
        DurableState::Blocked => DurableStateCode::Blocked,
        DurableState::AwaitingApproval => DurableStateCode::AwaitingApproval,
        DurableState::Enabled => DurableStateCode::Enabled,
        DurableState::Paused => DurableStateCode::Paused,
        DurableState::Disabled => DurableStateCode::Disabled,
        DurableState::Parked => DurableStateCode::Parked,
        DurableState::Allowed => DurableStateCode::Allowed,
        DurableState::Denied => DurableStateCode::Denied,
        DurableState::Retrying => DurableStateCode::Retrying,
        DurableState::Recovered => DurableStateCode::Recovered,
        DurableState::Finalized => DurableStateCode::Finalized,
        DurableState::Advanced => DurableStateCode::Advanced,
        DurableState::Deduplicated => DurableStateCode::Deduplicated,
        DurableState::Expired => DurableStateCode::Expired,
        DurableState::Acknowledged => DurableStateCode::Acknowledged,
        DurableState::Completed => DurableStateCode::Completed,
        DurableState::Succeeded => DurableStateCode::Succeeded,
        DurableState::Failed => DurableStateCode::Failed,
        DurableState::Cancelled => DurableStateCode::Cancelled,
        DurableState::Interrupted => DurableStateCode::Interrupted,
    }
}

fn validate_capture_binding(
    report: &CampaignReport,
    definition: &ProbeDefinition,
    probe: &ProbeResult,
    capture: &PersistentAgentCapture,
) -> Result<()> {
    if capture.campaign.campaign_id != opaque_durable_id(&report.campaign_id)
        || capture.campaign.repository_commit != report.repository_commit
        || capture.campaign.dirty != report.repository_dirty
    {
        bail!("capture campaign identity does not match its report");
    }
    validate_capture_durable_identity_binding(probe, capture)?;
    let provider = report
        .provider
        .as_ref()
        .context("capture has no bound provider summary")?;
    let route_matches = matches!(
        (&capture.provider.route_class, provider.route_class),
        (
            ProviderRouteClass::GrokBuildProxy,
            ProviderRouteCode::GrokBuildProxy
        ) | (ProviderRouteClass::XaiApi, ProviderRouteCode::XaiApi)
            | (
                ProviderRouteClass::CompatibleGateway,
                ProviderRouteCode::CompatibleGateway
            )
    );
    let credential_matches = matches!(
        (
            &capture.provider.credential_method,
            provider.credential_method
        ),
        (
            CredentialMethodClass::GrokBuildOidc,
            CredentialMethodCode::GrokBuildOidc
        ) | (
            CredentialMethodClass::ApiKeyReference,
            CredentialMethodCode::ApiKeyReference
        ) | (
            CredentialMethodClass::ManagedProviderReference,
            CredentialMethodCode::ManagedProviderReference
        )
    );
    if !route_matches
        || !credential_matches
        || capture.provider.model_identity != provider.model_identity
        || capture.provider.endpoint_fingerprint != provider.endpoint_fingerprint
    {
        bail!("capture provider identity does not match its report");
    }
    let bounds = &definition.bounds;
    let captured = &capture.budgets;
    if captured.bound_profile != bounds.bound_profile
        || captured.max_run_tokens != bounds.max_run_tokens
        || captured.max_total_tokens != bounds.max_total_tokens
        || captured.max_provider_requests != bounds.max_provider_requests
        || captured.max_continuations != bounds.max_continuations
        || captured.max_duration_seconds != bounds.max_duration_seconds
        || captured.max_raw_artifact_bytes != bounds.max_artifact_bytes
        || captured.max_response_bytes_per_request != bounds.max_response_bytes_per_request
    {
        bail!("capture budgets differ from the selected probe contract");
    }
    if probe.status == ProbeStatus::Passed
        && (!provider.observation_complete
            || !provider.authoritative_usage_complete
            || provider.observation_dropped_records > 0
            || capture.checks.is_empty()
            || capture.checks.iter().any(|check| !check.passed))
    {
        bail!("passing provider capture has incomplete checks, usage, or observation");
    }
    Ok(())
}

fn validate_capture_durable_identity_binding(
    probe: &ProbeResult,
    capture: &PersistentAgentCapture,
) -> Result<()> {
    if capture.durable_states.is_empty() {
        bail!("report capture has no durable Run partition");
    }
    let has_id = |kind: DurableIdKind, value: &str| {
        probe
            .opaque_ids
            .iter()
            .any(|candidate| candidate.kind == kind && candidate.value == value)
    };
    for state in &capture.durable_states {
        validate_capture_state_ids(state)?;
        if !has_id(DurableIdKind::Agent, &state.agent_id)
            || !has_id(DurableIdKind::Session, &state.lane_id)
            || !has_id(DurableIdKind::Run, &state.run_id)
        {
            bail!("capture durable identity is not bound to its probe evidence");
        }
        if let Some(parent) = &state.parent_run_id {
            if !has_id(DurableIdKind::Run, parent) {
                bail!("capture parent Run is not bound to its probe evidence");
            }
        }
    }
    Ok(())
}

fn validate_capture_state_ids(state: &grokptah_agent_bridge::DurableStateEvidence) -> Result<()> {
    for value in [&state.agent_id, &state.lane_id, &state.run_id] {
        validate_opaque_id(value)?;
    }
    if let Some(parent) = &state.parent_run_id {
        validate_opaque_id(parent)?;
    }
    if let Some(checkpoint) = &state.checkpoint_id {
        validate_opaque_id(checkpoint)?;
    }
    Ok(())
}

fn add_capture_actuals(aggregate: &mut CampaignActuals, actuals: &CampaignActuals) -> Result<()> {
    aggregate.total_tokens = aggregate
        .total_tokens
        .checked_add(actuals.total_tokens)
        .context("capture token actual overflow")?;
    aggregate.provider_requests = aggregate
        .provider_requests
        .checked_add(actuals.provider_requests)
        .context("capture request actual overflow")?;
    aggregate.continuations = aggregate
        .continuations
        .checked_add(actuals.continuations)
        .context("capture continuation actual overflow")?;
    aggregate.duration_seconds = aggregate.duration_seconds.max(actuals.duration_seconds);
    aggregate.raw_artifact_bytes = aggregate
        .raw_artifact_bytes
        .checked_add(actuals.raw_artifact_bytes)
        .context("capture artifact actual overflow")?;
    Ok(())
}

fn validate_diagnostics(values: &[DiagnosticCode], status: ProbeStatus) -> Result<()> {
    if values.is_empty() || values.len() > 32 {
        bail!("diagnostic count is outside its bound");
    }
    let unique: BTreeSet<_> = values.iter().copied().collect();
    if unique.len() != values.len() {
        bail!("diagnostic code is repeated");
    }
    if status == ProbeStatus::Passed {
        if values != [DiagnosticCode::Ok] {
            bail!("passed evidence must contain only the ok diagnostic");
        }
    } else if values.contains(&DiagnosticCode::Ok) {
        bail!("non-passing evidence contains the ok diagnostic");
    }
    Ok(())
}

fn validate_provider(provider: &ProviderSummary) -> Result<()> {
    validate_model_identity(&provider.model_identity)?;
    validate_hash(&provider.endpoint_fingerprint)?;
    validate_counter(provider.observation_dropped_records)?;
    match (provider.route_class, provider.credential_method) {
        (ProviderRouteCode::GrokBuildProxy, CredentialMethodCode::GrokBuildOidc)
        | (ProviderRouteCode::XaiApi, CredentialMethodCode::ApiKeyReference)
        | (ProviderRouteCode::CompatibleGateway, CredentialMethodCode::ManagedProviderReference) => {
        }
        _ => bail!("provider route and credential method are inconsistent"),
    }
    match provider.route_class {
        ProviderRouteCode::GrokBuildProxy | ProviderRouteCode::XaiApi => {
            if !provider.model_identity.starts_with("grok-") {
                bail!("public xAI route must retain a public grok-* model identity");
            }
            let route = match provider.route_class {
                ProviderRouteCode::GrokBuildProxy => ProviderRouteClass::GrokBuildProxy,
                ProviderRouteCode::XaiApi => ProviderRouteClass::XaiApi,
                ProviderRouteCode::CompatibleGateway => unreachable!(),
            };
            if public_xai_endpoint_fingerprint(&route).as_deref()
                != Some(provider.endpoint_fingerprint.as_str())
            {
                bail!("public provider endpoint fingerprint is not allowlisted");
            }
        }
        ProviderRouteCode::CompatibleGateway if !provider.model_identity.starts_with("opaque-") => {
            bail!("compatible gateway model identity is not opaque")
        }
        _ => {}
    }
    Ok(())
}

fn validate_model_identity(value: &str) -> Result<()> {
    if value.starts_with("opaque-") {
        return validate_opaque_id(value);
    }
    if !(6..=80).contains(&value.len())
        || !value.starts_with("grok-")
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        })
    {
        bail!("provider model identity is neither public Grok nor opaque");
    }
    Ok(())
}

fn validate_catalog_scenario_id(value: &str) -> Result<()> {
    if !PERSISTENT_AGENT_SCENARIO_IDS.contains(&value) {
        bail!("report references an unknown authoritative catalog scenario");
    }
    Ok(())
}

fn validate_opaque_id(value: &str) -> Result<()> {
    if value.len() != 71
        || !value.starts_with("opaque-")
        || !value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("durable identity is not an opaque sha256 label");
    }
    Ok(())
}

fn validate_commit(value: &str) -> Result<()> {
    if !(7..=64).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("repository commit is invalid");
    }
    Ok(())
}

fn parse_timestamp(value: &str) -> Result<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value).map_err(|_| anyhow::anyhow!("timestamp is not RFC3339"))
}

fn validate_hash(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid lowercase sha256 digest");
    }
    Ok(())
}

fn validate_counter(value: u64) -> Result<()> {
    if value > MAX_COUNTER_VALUE {
        bail!("evidence counter exceeds its structural bound");
    }
    Ok(())
}

fn validate_counters(counters: &EvidenceCounters) -> Result<()> {
    for value in [
        counters.events,
        counters.tool_calls,
        counters.permission_requests,
        counters.errors,
        counters.provider_attempts,
        counters.reconnects,
        counters.restarts,
        counters.dropped_records,
    ] {
        validate_counter(value)?;
    }
    Ok(())
}

fn checked_add_counters(
    left: &EvidenceCounters,
    right: &EvidenceCounters,
) -> Result<EvidenceCounters> {
    let add = |a: u64, b: u64| -> Result<u64> {
        let sum = a.checked_add(b).context("evidence counter overflow")?;
        validate_counter(sum)?;
        Ok(sum)
    };
    Ok(EvidenceCounters {
        events: add(left.events, right.events)?,
        tool_calls: add(left.tool_calls, right.tool_calls)?,
        permission_requests: add(left.permission_requests, right.permission_requests)?,
        errors: add(left.errors, right.errors)?,
        provider_attempts: add(left.provider_attempts, right.provider_attempts)?,
        reconnects: add(left.reconnects, right.reconnects)?,
        restarts: add(left.restarts, right.restarts)?,
        dropped_records: add(left.dropped_records, right.dropped_records)?,
    })
}

fn count_status(probes: &[ProbeResult], status: ProbeStatus) -> Result<u32> {
    u32::try_from(probes.iter().filter(|probe| probe.status == status).count())
        .context("probe status count overflow")
}

fn canonical_regular_directory(path: &Path) -> Result<PathBuf> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| anyhow::anyhow!("campaign root is unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("campaign root must be a non-symlink directory");
    }
    dunce::canonicalize(path).map_err(|_| anyhow::anyhow!("campaign root cannot be canonicalized"))
}

fn verify_artifact(root: &Path, reference: &ArtifactReference, max_bytes: u64) -> Result<Vec<u8>> {
    validate_artifact_reference(reference)?;
    if reference.bytes > max_bytes {
        bail!("referenced artifact exceeds its type-specific byte bound");
    }
    let relative = Path::new(&reference.relative_path);
    let mut candidate = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("artifact path contains a non-normal component");
        };
        candidate.push(part);
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|_| anyhow::anyhow!("referenced artifact is unavailable"))?;
        if metadata.file_type().is_symlink() {
            bail!("artifact path contains a symbolic link");
        }
    }
    let canonical = dunce::canonicalize(&candidate)
        .map_err(|_| anyhow::anyhow!("referenced artifact cannot be canonicalized"))?;
    if !canonical.starts_with(root) {
        bail!("referenced artifact resolves outside the campaign root");
    }
    let metadata_before = canonical
        .metadata()
        .map_err(|_| anyhow::anyhow!("referenced artifact metadata is unavailable"))?;
    if !metadata_before.is_file() || metadata_before.len() != reference.bytes {
        bail!("referenced artifact type or size does not match the report");
    }
    let read_limit = reference
        .bytes
        .checked_add(1)
        .context("artifact read limit overflow")?;
    let mut bytes = Vec::with_capacity(reference.bytes as usize);
    let file = fs::File::open(&canonical)
        .map_err(|_| anyhow::anyhow!("referenced artifact cannot be opened"))?;
    let metadata_open = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("opened artifact metadata is unavailable"))?;
    if !metadata_open.is_file() || !same_file(&metadata_before, &metadata_open) {
        bail!("referenced artifact changed while it was being opened");
    }
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("referenced artifact cannot be read"))?;
    if bytes.len() as u64 != reference.bytes {
        bail!("referenced artifact changed while being verified");
    }
    if format!("{:x}", Sha256::digest(&bytes)) != reference.sha256 {
        bail!("referenced artifact digest does not match the report");
    }
    Ok(bytes)
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

    fn skipped_probe() -> ProbeResult {
        ProbeResult::skipped(
            "native-policy-default-off-v1",
            vec!["spec-revision-001".into()],
            DiagnosticCode::ManagedExecutionUnavailable,
        )
    }

    fn report(root: &Path) -> CampaignReport {
        let manifest_path = root.join("contract/manifest.json");
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(&manifest_path, crate::manifest::BUNDLED_MANIFEST).unwrap();
        let manifest_sha256 = format!("{:x}", Sha256::digest(crate::manifest::BUNDLED_MANIFEST));
        let now = "2026-08-19T12:00:00Z".to_string();
        CampaignReport {
            schema: LAB_REPORT_SCHEMA.into(),
            campaign_id: "certification-smoke-v1".into(),
            run_id: "run-0001".into(),
            manifest_sha256: manifest_sha256.clone(),
            manifest_artifact: ArtifactReference {
                relative_path: "contract/manifest.json".into(),
                sha256: manifest_sha256,
                bytes: crate::manifest::BUNDLED_MANIFEST.len() as u64,
            },
            selected_probe_ids: vec!["native-policy-default-off-v1".into()],
            catalog_schema: CATALOG_SCHEMA.into(),
            repository_commit: "8ea4b779".into(),
            repository_dirty: false,
            started_at: now.clone(),
            finished_at: now,
            runtime_mode: RuntimeMode::Offline,
            transport_mode: TransportMode::LocalLaunch,
            provider: None,
            provider_actuals: None,
            discovered_tool_count: 70,
            summary: ReportSummary::default(),
            counters: EvidenceCounters::default(),
            probes: vec![skipped_probe()],
            certified: false,
            redaction: RedactionMetadata {
                policy: RedactionPolicy::PositiveSchemaV1,
                fields_omitted: 0,
                strings_truncated: 0,
                reasoning_records_dropped: 0,
                binary_records_dropped: 0,
                forbidden_scan_passed: true,
            },
            truncation: TruncationMetadata {
                bytes_written: crate::manifest::BUNDLED_MANIFEST.len() as u64,
                artifact_budget_bytes: 128 * 1024 * 1024,
                ..TruncationMetadata::default()
            },
        }
    }

    #[test]
    fn report_counts_and_semantics_are_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_ok());
        value.summary.passed = 1;
        assert!(value.validate_at(root.path()).is_err());

        let mut value = report(root.path());
        value.probes[0].supported = true;
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());

        let mut value = report(root.path());
        value.redaction.forbidden_scan_passed = false;
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());
    }

    #[test]
    fn raw_or_credential_shaped_durable_ids_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        value.probes[0].transitions.push(TransitionEvidence {
            entity: EntityKind::Run,
            from: DurableStateCode::Queued,
            to: DurableStateCode::Running,
            opaque_id: Some("sk-secret-looking-actual-id".into()),
        });
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());
        assert!(opaque_durable_id("actual-run-id").starts_with("opaque-"));
    }

    #[test]
    fn capture_durable_state_rejects_actual_ids_before_report_binding() {
        let opaque = |value: &str| opaque_durable_id(value);
        let mut state = grokptah_agent_bridge::DurableStateEvidence {
            role: grokptah_agent_bridge::certification::DurableEvidenceRole::Primary,
            agent_id: opaque("agent"),
            agent_spec_revision: 1,
            lane_id: opaque("lane"),
            run_id: opaque("run"),
            parent_run_id: Some(opaque("parent")),
            checkpoint_id: Some(opaque("checkpoint")),
            continuation_input_hash: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            terminal_state: "completed".into(),
            stop_cause: None,
        };
        assert!(validate_capture_state_ids(&state).is_ok());
        state.run_id = "actual-run-identifier".into();
        assert!(validate_capture_state_ids(&state).is_err());
    }

    #[test]
    fn provider_catalog_timestamp_and_commit_fields_are_typed() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        value.catalog_schema = "unknown".into();
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());

        let mut value = report(root.path());
        value.started_at = "not-a-time".into();
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());

        let mut value = report(root.path());
        value.repository_commit = "not-a-commit".into();
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());
    }

    #[test]
    fn capture_boolean_cannot_substitute_for_root_bound_verification() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        value.probes[0].capture_refs.push(CaptureReference {
            catalog_scenario_id: "endurance-finite-runs-001".into(),
            artifact: ArtifactReference {
                relative_path: "missing-capture.json".into(),
                sha256: "b".repeat(64),
                bytes: 10,
            },
            capture_schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
        });
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());
    }

    #[test]
    fn counters_and_artifact_budgets_do_not_saturate() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        value.probes[0].counters.events = MAX_COUNTER_VALUE + 1;
        assert!(value.recompute_summary().is_err());

        let mut value = report(root.path());
        value.truncation.artifact_budget_bytes = 0;
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());
    }

    #[test]
    fn omitted_selected_probe_and_all_skipped_certification_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        value
            .selected_probe_ids
            .push("core-service-readiness-v1".into());
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());

        let mut value = report(root.path());
        value.certified = true;
        value.recompute_summary().unwrap();
        assert!(value.validate_at(root.path()).is_err());
    }

    #[test]
    fn provider_scope_cannot_pass_without_capture_usage_and_attempt_evidence() {
        let root = tempfile::tempdir().unwrap();
        let mut value = report(root.path());
        let manifest = CampaignManifest::bundled().unwrap();
        let definition = manifest.probe("core-bounded-run-terminal-v1").unwrap();
        let trace = StructuralTrace {
            schema: LAB_TRACE_SCHEMA.into(),
            probe_id: definition.id.clone(),
            records: vec![TraceRecord {
                ordinal: 1,
                operation: TraceOperationCode::Oracle,
                argument_fields: vec![],
                diagnostic: Some(DiagnosticCode::Ok),
                sequence: None,
            }],
            truncated: false,
            dropped_records: 0,
        };
        let trace_bytes = trace.validate().unwrap();
        fs::create_dir_all(root.path().join("traces")).unwrap();
        fs::write(root.path().join("traces/provider.json"), &trace_bytes).unwrap();
        value.runtime_mode = RuntimeMode::Live;
        value.selected_probe_ids = vec![definition.id.clone()];
        value.provider = Some(ProviderSummary {
            route_class: ProviderRouteCode::GrokBuildProxy,
            model_identity: "grok-build".into(),
            credential_method: CredentialMethodCode::GrokBuildOidc,
            endpoint_fingerprint: public_xai_endpoint_fingerprint(
                &ProviderRouteClass::GrokBuildProxy,
            )
            .unwrap(),
            observation_complete: true,
            authoritative_usage_complete: true,
            observation_dropped_records: 0,
        });
        value.probes = vec![ProbeResult {
            probe_id: definition.id.clone(),
            catalog_scenario_ids: definition.catalog_scenario_ids.clone(),
            status: ProbeStatus::Passed,
            supported: true,
            failure_class: FailureClass::None,
            diagnostics: vec![DiagnosticCode::Ok],
            verified_actions: definition.actions.clone(),
            verified_oracles: definition.oracle_codes.clone(),
            phases: vec![PhaseResult {
                phase: PhaseCode::Oracle,
                status: ProbeStatus::Passed,
                elapsed_millis: 1,
                diagnostics: vec![DiagnosticCode::Ok],
            }],
            transitions: vec![
                TransitionEvidence {
                    entity: EntityKind::Run,
                    from: DurableStateCode::Queued,
                    to: DurableStateCode::Running,
                    opaque_id: Some(opaque_durable_id("run")),
                },
                TransitionEvidence {
                    entity: EntityKind::Run,
                    from: DurableStateCode::Running,
                    to: DurableStateCode::Completed,
                    opaque_id: Some(opaque_durable_id("run")),
                },
            ],
            counters: EvidenceCounters::default(),
            reconnect: ReconnectEvidence::default(),
            restart: RestartEvidence::default(),
            opaque_ids: Vec::new(),
            trace: Some(ArtifactReference {
                relative_path: "traces/provider.json".into(),
                sha256: format!("{:x}", Sha256::digest(&trace_bytes)),
                bytes: trace_bytes.len() as u64,
            }),
            capture_refs: Vec::new(),
            elapsed_millis: 1,
        }];
        value.recompute_summary().unwrap();
        value.certified = true;
        assert!(value.validate_at(root.path()).is_err());
    }

    #[test]
    fn live_native_probe_cannot_pass_without_provider_capture() {
        let manifest = CampaignManifest::bundled().unwrap();
        let definition = manifest.probe("native-no-duplicate-run-v1").unwrap();
        let mut probe = ProbeResult::skipped(
            definition.id.clone(),
            definition.catalog_scenario_ids.clone(),
            DiagnosticCode::Ok,
        );
        probe.status = ProbeStatus::Passed;
        probe.supported = true;
        probe.failure_class = FailureClass::None;
        probe.diagnostics = vec![DiagnosticCode::Ok];
        probe.verified_actions = definition.actions.clone();
        probe.verified_oracles = definition.oracle_codes.clone();
        probe.phases = vec![PhaseResult {
            phase: PhaseCode::Oracle,
            status: ProbeStatus::Passed,
            elapsed_millis: 1,
            diagnostics: vec![DiagnosticCode::Ok],
        }];
        probe.transitions = vec![
            TransitionEvidence {
                entity: EntityKind::ExecutionIntent,
                from: DurableStateCode::Absent,
                to: DurableStateCode::Admitted,
                opaque_id: None,
            },
            TransitionEvidence {
                entity: EntityKind::Attempt,
                from: DurableStateCode::Absent,
                to: DurableStateCode::Leased,
                opaque_id: None,
            },
            TransitionEvidence {
                entity: EntityKind::Run,
                from: DurableStateCode::Absent,
                to: DurableStateCode::Queued,
                opaque_id: None,
            },
        ];
        probe.trace = Some(ArtifactReference {
            relative_path: "traces/native-no-duplicate-run-v1.json".into(),
            sha256: "a".repeat(64),
            bytes: 1,
        });
        assert!(validate_probe_against_definition(&probe, definition).is_err());
    }

    #[test]
    fn structural_trace_has_only_typed_operations_and_fields() {
        let trace = StructuralTrace {
            schema: LAB_TRACE_SCHEMA.into(),
            probe_id: "core-ready-v1".into(),
            records: vec![TraceRecord {
                ordinal: 1,
                operation: TraceOperationCode::ListTools,
                argument_fields: vec![],
                diagnostic: Some(DiagnosticCode::Ok),
                sequence: None,
            }],
            truncated: false,
            dropped_records: 0,
        };
        assert!(trace.validate().is_ok());
    }
}
