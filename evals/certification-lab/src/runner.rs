//! Campaign orchestration over the public MCP service contract.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use grokptah_agent_bridge::provider_observation::{
    EvidenceMode, InMemoryObservationRecorder, OpaqueScopeId, ProviderObservationSession,
};
use grokptah_agent_bridge::{
    attest_grok_build_oidc_with_min_validity, CredentialMethodClass, McpControlClient,
    ProviderRouteClass, PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

use crate::artifact::{SafeOutputRoot, DEFAULT_OUTPUT_RELATIVE_PATH};
use crate::capture::{build_capture, CaptureRunSet};
use crate::local_service::{
    validate_public_model, LocalService, LocalServiceConfig, LocalServiceMode, DEFAULT_LIVE_MODEL,
};
use crate::manifest::{
    AttachAccess, CampaignManifest, ProbeDefinition, ProbeEffect, ProbeScope, RunnerCapability,
};
use crate::probes::{
    execute_minimal_probe, execute_native_interruption_retry_probe, execute_native_restart_probe,
    execute_restart_probe, has_implementation, ProbeExecution,
};
use crate::report::{
    ArtifactReference, CampaignReport, CaptureReference, CredentialMethodCode, DiagnosticCode,
    EvidenceCounters, FailureClass, ProbeResult, ProbeStatus, ProviderRouteCode, ProviderSummary,
    RedactionMetadata, RedactionPolicy, ReportSummary, RuntimeMode, TransportMode,
    TruncationMetadata, CATALOG_SCHEMA,
};
use crate::LAB_REPORT_SCHEMA;

pub const ATTACH_TOKEN_ENV: &str = "GROKPTAH_CERT_SERVICE_TOKEN";
pub const LIVE_OPT_IN_ENV: &str = "GROKPTAH_LIVE_CERT";
const LIVE_PREFLIGHT_VALIDITY_SECONDS: i64 = 10 * 60;
const LIVE_ROUTE_OVERRIDE_ENVS: &[&str] =
    &["XAI_API_KEY", "XAI_API_BASE", "GROKPTAH_TOKEN_COMMAND"];
pub const DEFAULT_SMOKE_PROBES: &[&str] = &[
    "core-service-readiness-v1",
    "core-agent-identity-v1",
    "work-idempotency-conflict-v1",
    "routine-manual-activation-v1",
    "coordinator-parent-child-work-v1",
    "core-continuation-resume-v1",
    "native-policy-default-off-v1",
];

#[derive(Debug, Clone)]
pub enum TransportConfig {
    Local,
    Attach {
        endpoint: String,
        token_env: String,
        workspace: Option<String>,
        allow_mutations: bool,
        disposable_target_acknowledged: bool,
    },
}

#[derive(Debug, Clone)]
pub struct CampaignOptions {
    pub repository_root: PathBuf,
    pub manifest_path: PathBuf,
    pub output_root: PathBuf,
    pub runtime_mode: RuntimeMode,
    pub transport: TransportConfig,
    pub selected_probe_ids: Vec<String>,
    pub artifact_budget_bytes: u64,
    pub model: String,
    pub live_model_explicit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreflightSummary {
    pub manifest_valid: bool,
    pub runtime_mode: RuntimeMode,
    pub transport_mode: TransportMode,
    pub live_opt_in_valid: bool,
    pub service_reachable: bool,
    pub discovered_tool_count: u32,
    pub selected_probe_count: u32,
    pub unsupported_probe_count: u32,
    pub indeterminate_probe_count: u32,
    pub endpoint_fingerprint: Option<String>,
    pub model_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignCompletion {
    pub campaign_id: String,
    pub summary: ReportSummary,
    pub failure_classes: Vec<FailureClass>,
    pub report_sha256: String,
    pub certified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CampaignPartial {
    schema: String,
    campaign_id: String,
    manifest_sha256: String,
    selected_probe_ids: Vec<String>,
    completed_probe_ids: Vec<String>,
    runtime_mode: RuntimeMode,
    transport_mode: TransportMode,
    updated_at: String,
    resume_supported: bool,
}

enum ActiveTransport {
    Local {
        service: Box<LocalService>,
        _layout: TempDir,
        workspace: String,
    },
    Attach {
        endpoint: String,
        token: String,
        workspace: Option<String>,
    },
}

impl ActiveTransport {
    fn mode(&self) -> TransportMode {
        match self {
            Self::Local { .. } => TransportMode::LocalLaunch,
            Self::Attach { .. } => TransportMode::Attach,
        }
    }

    fn workspace(&self) -> Option<&str> {
        match self {
            Self::Local { workspace, .. } => Some(workspace),
            Self::Attach { workspace, .. } => workspace.as_deref(),
        }
    }

    fn client(&self) -> McpControlClient {
        match self {
            Self::Local { service, .. } => service.client(),
            Self::Attach {
                endpoint, token, ..
            } => McpControlClient::new(endpoint.clone(), token.clone()),
        }
    }

    async fn stop(self) {
        if let Self::Local { service, .. } = self {
            service.stop().await;
        }
    }
}

pub fn default_options(repository_root: &Path) -> CampaignOptions {
    CampaignOptions {
        repository_root: repository_root.to_path_buf(),
        manifest_path: repository_root.join("evals/certification-lab/campaign.v1.json"),
        output_root: repository_root.join(DEFAULT_OUTPUT_RELATIVE_PATH),
        runtime_mode: RuntimeMode::Offline,
        transport: TransportConfig::Local,
        selected_probe_ids: DEFAULT_SMOKE_PROBES
            .iter()
            .map(|value| (*value).into())
            .collect(),
        artifact_budget_bytes: 128 * 1024 * 1024,
        model: DEFAULT_LIVE_MODEL.into(),
        live_model_explicit: false,
    }
}

pub async fn preflight(options: &CampaignOptions) -> Result<PreflightSummary> {
    validate_options(options)?;
    let _output = SafeOutputRoot::open(
        &options.output_root,
        &options.repository_root,
        None,
        options.artifact_budget_bytes,
    )?;
    let manifest =
        CampaignManifest::load_checked(&options.manifest_path, &options.repository_root)?;
    validate_artifact_budget(options, &manifest)?;
    let selected = manifest.select_probes(&options.selected_probe_ids)?;
    let (transport, recorder) = start_transport(options, &manifest, None).await?;
    let mut client = transport.client();
    let initialized = client
        .initialize()
        .await
        .map_err(|_| anyhow::anyhow!("mcp_initialize_failed"))?;
    validate_service_identity(&initialized)?;
    let tools = client
        .list_tools()
        .await
        .map_err(|_| anyhow::anyhow!("tool_discovery_failed"))?;
    let names: HashSet<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    let mut capabilities = runner_capabilities(options, &transport, recorder.is_some());
    augment_discovered_capabilities(&mut client, &names, &mut capabilities).await;
    let mut unsupported = 0usize;
    let mut indeterminate = 0usize;
    for probe in &selected {
        match classify_probe_availability(probe, options, &transport, &names, &capabilities) {
            Some(result) if result.status == ProbeStatus::Skipped => {
                unsupported = unsupported
                    .checked_add(1)
                    .context("unsupported probe count overflow")?;
            }
            Some(_) => {
                indeterminate = indeterminate
                    .checked_add(1)
                    .context("indeterminate probe count overflow")?;
            }
            None => {}
        }
    }
    let _ = client.close_session().await;
    let mode = transport.mode();
    let endpoint_fingerprint = match &options.transport {
        TransportConfig::Attach { endpoint, .. } => Some(endpoint_fingerprint(endpoint)?),
        TransportConfig::Local => None,
    };
    let summary = PreflightSummary {
        manifest_valid: true,
        runtime_mode: options.runtime_mode,
        transport_mode: mode,
        live_opt_in_valid: live_gate_valid(options.runtime_mode),
        service_reachable: true,
        discovered_tool_count: u32::try_from(tools.len()).context("tool count overflow")?,
        selected_probe_count: u32::try_from(selected.len()).context("probe count overflow")?,
        unsupported_probe_count: u32::try_from(unsupported)
            .context("unsupported probe count overflow")?,
        indeterminate_probe_count: u32::try_from(indeterminate)
            .context("indeterminate probe count overflow")?,
        endpoint_fingerprint,
        model_identity: match options.runtime_mode {
            RuntimeMode::Live if matches!(&options.transport, TransportConfig::Local) => {
                Some(options.model.clone())
            }
            _ => None,
        },
    };
    drop(recorder);
    transport.stop().await;
    Ok(summary)
}

pub async fn run_campaign(options: &CampaignOptions) -> Result<CampaignCompletion> {
    validate_options(options)?;
    let manifest =
        CampaignManifest::load_checked(&options.manifest_path, &options.repository_root)?;
    let selected = manifest.select_probes(&options.selected_probe_ids)?;
    let repository_commit = repository_commit(&options.repository_root)?;
    let repository_dirty = repository_dirty(&options.repository_root)?;
    if options.runtime_mode == RuntimeMode::Live {
        let selected_duration_seconds = selected.iter().try_fold(0u64, |total, definition| {
            total
                .checked_add(definition.bounds.max_duration_seconds)
                .context("selected campaign validity window overflow")
        })?;
        let required_validity = ChronoDuration::seconds(
            i64::try_from(selected_duration_seconds)
                .context("selected campaign validity window overflow")?,
        );
        attest_grok_build_oidc_with_min_validity(&options.model, required_validity)
            .map_err(|error| anyhow::anyhow!("live_oidc_attestation_{}", error.code()))?;
    }
    validate_artifact_budget(options, &manifest)?;
    // Establish sentinel ownership, confinement, and free-space headroom
    // before starting even the deterministic local host. The second open
    // below additionally checks the freshly created disposable workspace.
    let output = SafeOutputRoot::open(
        &options.output_root,
        &options.repository_root,
        None,
        options.artifact_budget_bytes,
    )?;
    let selected_probe_ids: Vec<String> = selected
        .iter()
        .map(|definition| definition.id.clone())
        .collect();
    let campaign_id = format!(
        "cert-{}-{}",
        Utc::now().format("%Y%m%d%H%M%S"),
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (mut transport, recorder) =
        start_transport(options, &manifest, Some(campaign_id.as_str())).await?;
    let active_workspace = match &transport {
        ActiveTransport::Local { workspace, .. } => Some(Path::new(workspace)),
        ActiveTransport::Attach { .. } => None,
    };
    let output = match active_workspace {
        Some(workspace) => SafeOutputRoot::open(
            &options.output_root,
            &options.repository_root,
            Some(workspace),
            options.artifact_budget_bytes,
        )?,
        None => output,
    };
    let artifacts = output.create_campaign(&campaign_id)?;

    let manifest_bytes = std::fs::read(&options.manifest_path).context("read checked manifest")?;
    if manifest_bytes.len() > crate::manifest::MAX_MANIFEST_BYTES {
        bail!("manifest_too_large");
    }
    let manifest_digest = artifacts.write_final("contract/manifest.json", &manifest_bytes)?;

    let mut client = transport.client();
    let initialized = client
        .initialize()
        .await
        .map_err(|_| anyhow::anyhow!("mcp_initialize_failed"))?;
    validate_service_identity(&initialized)?;
    let tools = client
        .list_tools()
        .await
        .map_err(|_| anyhow::anyhow!("tool_discovery_failed"))?;
    let tool_names: HashSet<String> = tools.into_iter().map(|tool| tool.name).collect();
    let started_at = Utc::now();
    let mut report = CampaignReport {
        schema: LAB_REPORT_SCHEMA.into(),
        campaign_id: campaign_id.clone(),
        run_id: format!("run-{}", Uuid::new_v4().simple()),
        manifest_sha256: digest(&manifest_bytes),
        manifest_artifact: ArtifactReference {
            relative_path: manifest_digest.relative_path,
            sha256: manifest_digest.sha256,
            bytes: manifest_digest.bytes,
        },
        selected_probe_ids,
        catalog_schema: CATALOG_SCHEMA.into(),
        repository_commit,
        repository_dirty,
        started_at: started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        finished_at: started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        runtime_mode: options.runtime_mode,
        transport_mode: transport.mode(),
        provider: None,
        provider_actuals: None,
        discovered_tool_count: u32::try_from(tool_names.len()).context("tool count overflow")?,
        summary: ReportSummary::default(),
        counters: EvidenceCounters::default(),
        probes: Vec::with_capacity(selected.len()),
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
            traces_truncated: 0,
            records_dropped: 0,
            bytes_written: 0,
            artifact_budget_bytes: options.artifact_budget_bytes,
        },
    };
    write_partial_checkpoint(&artifacts, &report)?;

    let mut capabilities = runner_capabilities(options, &transport, recorder.is_some());
    augment_discovered_capabilities(&mut client, &tool_names, &mut capabilities).await;
    for definition in selected {
        let mut result = classify_probe_availability(
            definition,
            options,
            &transport,
            &tool_names,
            &capabilities,
        );
        if result.is_none() {
            let workspace = transport
                .workspace()
                .or_else(|| (definition.id == "core-service-readiness-v1").then_some(""))
                .map(str::to_owned);
            if let Some(workspace) = workspace {
                let mut execution = match &mut transport {
                    ActiveTransport::Local { service, .. }
                        if definition.id == "core-restart-durable-runs-events-v1" =>
                    {
                        match tokio::time::timeout(
                            Duration::from_secs(definition.bounds.max_duration_seconds),
                            execute_restart_probe(
                                definition,
                                service,
                                &workspace,
                                recorder.as_ref(),
                            ),
                        )
                        .await
                        {
                            Ok(execution) => execution,
                            Err(_) => ProbeExecution::timed_out(definition),
                        }
                    }
                    ActiveTransport::Local { service, .. }
                        if definition.id == "native-restart-intent-adoption-v1" =>
                    {
                        match tokio::time::timeout(
                            Duration::from_secs(definition.bounds.max_duration_seconds),
                            execute_native_restart_probe(
                                definition,
                                service,
                                &workspace,
                                recorder.as_ref(),
                            ),
                        )
                        .await
                        {
                            Ok(execution) => execution,
                            Err(_) => ProbeExecution::timed_out(definition),
                        }
                    }
                    ActiveTransport::Local { service, .. }
                        if definition.id == "native-interruption-retry-policy-v1" =>
                    {
                        match tokio::time::timeout(
                            Duration::from_secs(definition.bounds.max_duration_seconds),
                            execute_native_interruption_retry_probe(
                                definition,
                                service,
                                &workspace,
                                recorder.as_ref(),
                            ),
                        )
                        .await
                        {
                            Ok(execution) => execution,
                            Err(_) => ProbeExecution::timed_out(definition),
                        }
                    }
                    _ => match tokio::time::timeout(
                        Duration::from_secs(definition.bounds.max_duration_seconds),
                        execute_minimal_probe(
                            definition,
                            &mut client,
                            &workspace,
                            recorder.as_ref().and_then(|recorder| {
                                recorder
                                    .snapshot()
                                    .last()
                                    .map(|observation| observation.attempt_number() + 1)
                                    .or(Some(1))
                            }),
                            recorder.as_ref(),
                        ),
                    )
                    .await
                    {
                        Ok(execution) => execution,
                        Err(_) => ProbeExecution::timed_out(definition),
                    },
                };
                if matches!(
                    definition.id.as_str(),
                    "core-restart-durable-runs-events-v1"
                        | "native-restart-intent-adoption-v1"
                        | "native-interruption-retry-policy-v1"
                ) {
                    client = transport.client();
                    client
                        .initialize()
                        .await
                        .map_err(|_| anyhow::anyhow!("mcp_reinitialize_failed"))?;
                }
                if let Some(recorder) = recorder.as_ref() {
                    let scenario_run = execution.provider_run.clone();
                    let capture_provider_run = execution.capture_provider_run.take();
                    let capture_run_is_distinct =
                        capture_provider_run.as_ref().is_some_and(|capture_run| {
                            scenario_run
                                .as_ref()
                                .is_some_and(|scenario| scenario.run_id != capture_run.run_id)
                        });
                    if let Some(provider_run) = capture_provider_run.or(scenario_run.clone()) {
                        let capture_result: Result<grokptah_agent_bridge::PersistentAgentCapture> =
                            (|| {
                                let start = if capture_run_is_distinct {
                                    execution.capture_attempt_start.unwrap_or(1)
                                } else {
                                    execution.provider_attempt_start.unwrap_or(1)
                                };
                                let run_scope =
                                    OpaqueScopeId::from_stable_input(&provider_run.run_id);
                                let observations = recorder
                                    .snapshot()
                                    .into_iter()
                                    .filter(|observation| {
                                        observation.attempt_number() >= start
                                            && observation.scope().run_id() == &run_scope
                                    })
                                    .collect::<Vec<_>>();
                                let scenario_id = definition
                                    .catalog_scenario_ids
                                    .first()
                                    .context("provider_catalog_scenario_missing")?;
                                let recovery_runs = scenario_run
                                    .into_iter()
                                    .filter(|scenario| scenario.run_id != provider_run.run_id)
                                    .collect::<Vec<_>>();
                                let capture = build_capture(
                                    &campaign_id,
                                    scenario_id,
                                    &report.repository_commit,
                                    report.repository_dirty,
                                    &definition.bounds,
                                    &observations,
                                    CaptureRunSet {
                                        provider: &provider_run,
                                        recovery: &recovery_runs,
                                    },
                                )?;
                                capture.validate_complete_structural_evidence().map_err(
                                    |error| anyhow::anyhow!("capture_incomplete:{error}"),
                                )?;
                                Ok(capture)
                            })();
                        match capture_result {
                            Ok(capture) => {
                                let capture_bytes = serde_json::to_vec_pretty(&capture)
                                    .context("serialize provider capture")?;
                                let capture_digest = artifacts.write_final(
                                    format!("captures/{}.json", definition.id),
                                    &capture_bytes,
                                )?;
                                execution.result.capture_refs.push(CaptureReference {
                                    catalog_scenario_id: capture.campaign.scenario_id.clone(),
                                    artifact: ArtifactReference {
                                        relative_path: capture_digest.relative_path,
                                        sha256: capture_digest.sha256,
                                        bytes: capture_digest.bytes,
                                    },
                                    capture_schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
                                });
                                execution.result.counters.provider_attempts =
                                    u64::from(capture.actuals.provider_requests);
                                let provider = provider_summary(&capture);
                                if report.provider.is_some() {
                                    bail!(
                                    "multiple provider identities in one campaign are unsupported"
                                );
                                }
                                report.provider = Some(provider);
                                report.provider_actuals = Some(capture.actuals.clone());
                            }
                            Err(_) => {
                                execution.result = ProbeResult::indeterminate(
                                    definition.id.clone(),
                                    definition.catalog_scenario_ids.clone(),
                                    DiagnosticCode::CaptureInvalid,
                                );
                            }
                        }
                    }
                }
                let trace_bytes = execution.trace.validate()?;
                let trace_path = format!("traces/{}.json", definition.id);
                let digest = artifacts.write_final(&trace_path, &trace_bytes)?;
                execution.result.trace = Some(ArtifactReference {
                    relative_path: digest.relative_path,
                    sha256: digest.sha256,
                    bytes: digest.bytes,
                });
                result = Some(execution.result);
            } else {
                result = Some(ProbeResult::indeterminate(
                    definition.id.clone(),
                    definition.catalog_scenario_ids.clone(),
                    DiagnosticCode::ScopeRejected,
                ));
            }
        }
        report.probes.push(result.expect("probe result assigned"));
        report.finished_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        report.recompute_summary()?;
        report.truncation.bytes_written = artifacts.used_bytes()?;
        write_partial_checkpoint(&artifacts, &report)?;
    }

    let _ = client.close_session().await;
    // Observations are never converted into provider identity by presence
    // alone. A future enabled live path must first produce an attested,
    // complete PersistentAgentCapture and bind it to this report.
    drop(recorder);
    report.finished_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    report.recompute_summary()?;
    report.certified = report.summary.probes > 0
        && !report.repository_dirty
        && report.summary.passed == report.summary.probes
        && report.summary.failed == 0
        && report.summary.skipped == 0
        && report.summary.indeterminate == 0;
    report.truncation.bytes_written = artifacts.used_bytes()?;
    let final_bytes = report.validate_at(artifacts.path())?;
    let final_digest = artifacts.write_final("report.json", &final_bytes)?;
    artifacts.verify("report.json", final_digest.bytes, &final_digest.sha256)?;
    let installed =
        artifacts.bounded_read("report.json", crate::report::MAX_REPORT_BYTES as u64)?;
    let installed_report: CampaignReport = serde_json::from_slice(&installed)
        .map_err(|_| anyhow::anyhow!("installed_report_invalid"))?;
    installed_report.validate_at(artifacts.path())?;
    artifacts.mark_complete(&final_digest)?;
    transport.stop().await;
    let mut failure_classes = Vec::new();
    for probe in &report.probes {
        if probe.failure_class != FailureClass::None
            && !failure_classes.contains(&probe.failure_class)
        {
            failure_classes.push(probe.failure_class);
        }
    }
    Ok(CampaignCompletion {
        campaign_id,
        summary: report.summary,
        failure_classes,
        report_sha256: final_digest.sha256,
        certified: report.certified,
    })
}

fn write_partial_checkpoint(
    artifacts: &crate::artifact::CampaignArtifacts,
    report: &CampaignReport,
) -> Result<()> {
    let partial = CampaignPartial {
        schema: "grokptah.certification_campaign_partial.v1".into(),
        campaign_id: report.campaign_id.clone(),
        manifest_sha256: report.manifest_sha256.clone(),
        selected_probe_ids: report.selected_probe_ids.clone(),
        completed_probe_ids: report
            .probes
            .iter()
            .map(|probe| probe.probe_id.clone())
            .collect(),
        runtime_mode: report.runtime_mode,
        transport_mode: report.transport_mode,
        updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        // Safe action reconciliation and deterministic request-ID resume are
        // intentionally not claimed in this version.
        resume_supported: false,
    };
    let value = serde_json::to_value(&partial)?;
    grokptah_agent_bridge::scan_value_for_forbidden_data(&value)
        .map_err(|_| anyhow::anyhow!("partial_checkpoint_redaction_failed"))?;
    let bytes = serde_json::to_vec_pretty(&partial)?;
    if bytes.len() > crate::report::MAX_REPORT_BYTES {
        bail!("partial_checkpoint_too_large");
    }
    artifacts.write_partial("campaign.partial.json", &bytes)?;
    Ok(())
}

fn validate_artifact_budget(options: &CampaignOptions, manifest: &CampaignManifest) -> Result<()> {
    if options.artifact_budget_bytes == 0
        || options.artifact_budget_bytes > manifest.campaign_bounds.max_artifact_bytes
    {
        bail!("campaign_artifact_budget_exceeds_manifest");
    }
    Ok(())
}

fn classify_probe_availability(
    probe: &ProbeDefinition,
    options: &CampaignOptions,
    transport: &ActiveTransport,
    tools: &HashSet<String>,
    capabilities: &HashSet<RunnerCapability>,
) -> Option<ProbeResult> {
    classify_probe(probe, options, transport, tools, capabilities).or_else(|| {
        (!has_implementation(&probe.id)).then(|| {
            ProbeResult::indeterminate(
                probe.id.clone(),
                probe.catalog_scenario_ids.clone(),
                DiagnosticCode::ProbeImplementationUnavailable,
            )
        })
    })
}

fn classify_probe(
    probe: &ProbeDefinition,
    options: &CampaignOptions,
    transport: &ActiveTransport,
    tools: &HashSet<String>,
    capabilities: &HashSet<RunnerCapability>,
) -> Option<ProbeResult> {
    let eligible_mode = match options.runtime_mode {
        RuntimeMode::Offline => probe.eligibility.offline,
        RuntimeMode::Live => probe.eligibility.live,
    };
    if !eligible_mode {
        return Some(ProbeResult::skipped(
            probe.id.clone(),
            probe.catalog_scenario_ids.clone(),
            DiagnosticCode::ModeIneligible,
        ));
    }
    let eligible_transport = match transport {
        ActiveTransport::Local { .. } => probe.eligibility.local_launch,
        ActiveTransport::Attach { .. } => probe.eligibility.attach,
    };
    if !eligible_transport {
        return Some(ProbeResult::skipped(
            probe.id.clone(),
            probe.catalog_scenario_ids.clone(),
            DiagnosticCode::TransportIneligible,
        ));
    }
    if matches!(transport, ActiveTransport::Attach { .. })
        && probe.effect == ProbeEffect::Mutating
        && matches!(
            probe.eligibility.attach_access,
            AttachAccess::ExplicitMutationOptIn
        )
        && !matches!(
            &options.transport,
            TransportConfig::Attach {
                allow_mutations: true,
                ..
            }
        )
    {
        return Some(ProbeResult::skipped(
            probe.id.clone(),
            probe.catalog_scenario_ids.clone(),
            DiagnosticCode::AttachMutationOptInRequired,
        ));
    }
    if probe
        .required_tools
        .iter()
        .any(|tool| !tools.contains(tool))
    {
        let diagnostic = if probe.scope == ProbeScope::NativeManagedFuture {
            DiagnosticCode::ManagedExecutionUnavailable
        } else {
            DiagnosticCode::RequiredToolMissing
        };
        return Some(ProbeResult::skipped(
            probe.id.clone(),
            probe.catalog_scenario_ids.clone(),
            diagnostic,
        ));
    }
    let missing: Vec<_> = probe
        .required_capabilities
        .iter()
        .filter(|capability| !capabilities.contains(capability))
        .collect();
    if !missing.is_empty() {
        let incomplete = missing.iter().any(|capability| {
            matches!(
                capability,
                RunnerCapability::ProviderObservation | RunnerCapability::AuthoritativeUsage
            )
        });
        if incomplete {
            return Some(ProbeResult::indeterminate(
                probe.id.clone(),
                probe.catalog_scenario_ids.clone(),
                DiagnosticCode::ProviderObservationUnavailable,
            ));
        }
        let diagnostic = if missing
            .iter()
            .any(|capability| **capability == RunnerCapability::DeterministicClock)
        {
            DiagnosticCode::DeterministicClockUnavailable
        } else if missing
            .iter()
            .any(|capability| **capability == RunnerCapability::NativeManagedExecution)
        {
            DiagnosticCode::ManagedExecutionUnavailable
        } else {
            DiagnosticCode::CapabilityAbsent
        };
        return Some(ProbeResult::skipped(
            probe.id.clone(),
            probe.catalog_scenario_ids.clone(),
            diagnostic,
        ));
    }
    None
}

fn runner_capabilities(
    options: &CampaignOptions,
    transport: &ActiveTransport,
    has_observer: bool,
) -> HashSet<RunnerCapability> {
    let mut capabilities = HashSet::from([
        RunnerCapability::McpStreamableHttp,
        RunnerCapability::ClientReconnect,
        RunnerCapability::PersistentAgentEvidence,
    ]);
    if matches!(transport, ActiveTransport::Local { .. }) {
        capabilities.insert(RunnerCapability::DisposableWorkspace);
        capabilities.insert(RunnerCapability::LocalRestartControl);
        capabilities.insert(RunnerCapability::PreexistingTestAgents);
    }
    if options.runtime_mode == RuntimeMode::Live {
        capabilities.insert(RunnerCapability::LiveProviderRoute);
        if has_observer {
            capabilities.insert(RunnerCapability::ProviderObservation);
            capabilities.insert(RunnerCapability::AuthoritativeUsage);
        }
    }
    capabilities
}

fn provider_summary(capture: &grokptah_agent_bridge::PersistentAgentCapture) -> ProviderSummary {
    let route_class = match capture.provider.route_class {
        ProviderRouteClass::GrokBuildProxy => ProviderRouteCode::GrokBuildProxy,
        ProviderRouteClass::XaiApi => ProviderRouteCode::XaiApi,
        ProviderRouteClass::CompatibleGateway => ProviderRouteCode::CompatibleGateway,
    };
    let credential_method = match capture.provider.credential_method {
        CredentialMethodClass::GrokBuildOidc => CredentialMethodCode::GrokBuildOidc,
        CredentialMethodClass::ApiKeyReference => CredentialMethodCode::ApiKeyReference,
        CredentialMethodClass::ManagedProviderReference => {
            CredentialMethodCode::ManagedProviderReference
        }
    };
    ProviderSummary {
        route_class,
        model_identity: capture.provider.model_identity.clone(),
        credential_method,
        endpoint_fingerprint: capture.provider.endpoint_fingerprint.clone(),
        observation_complete: true,
        authoritative_usage_complete: capture
            .attempts
            .iter()
            .all(|attempt| attempt.usage.as_ref().is_some_and(|usage| usage.complete)),
        observation_dropped_records: 0,
    }
}

async fn augment_discovered_capabilities(
    client: &mut McpControlClient,
    tools: &HashSet<String>,
    capabilities: &mut HashSet<RunnerCapability>,
) {
    let managed_tools = [
        crate::manifest::FUTURE_SET_MANAGED_EXECUTION_TOOL,
        crate::manifest::FUTURE_GET_MANAGED_EXECUTION_TOOL,
        crate::manifest::FUTURE_AUTHORIZE_WORK_EXECUTION_TOOL,
        crate::manifest::FUTURE_RESOLVE_WORK_INPUT_TOOL,
        crate::manifest::FUTURE_LIST_EXECUTION_INTENTS_TOOL,
    ];
    if managed_tools.iter().any(|tool| !tools.contains(*tool)) {
        return;
    }
    let Ok(capacity) = client
        .call_tool("ptah_get_capacity", serde_json::json!({}))
        .await
    else {
        return;
    };
    if capacity.is_error
        || capacity.structured["health"]["nativeExecutor"]["enabled"].as_bool() != Some(true)
        || capacity.structured["health"]
            .get("nativeExecutorError")
            .is_some_and(|value| !value.is_null())
    {
        return;
    }
    capabilities.insert(RunnerCapability::NativeManagedExecution);
    capabilities.insert(RunnerCapability::ExplicitPermissionControl);
}

async fn start_transport(
    options: &CampaignOptions,
    manifest: &CampaignManifest,
    campaign_scope: Option<&str>,
) -> Result<(ActiveTransport, Option<InMemoryObservationRecorder>)> {
    match &options.transport {
        TransportConfig::Local => {
            let layout = tempfile::tempdir().context("create disposable certification layout")?;
            let home = layout.path().join("home");
            let workspace = layout.path().join("workspace");
            std::fs::create_dir(&workspace).context("create disposable scenario workspace")?;
            let workspace = dunce::canonicalize(workspace)
                .context("canonicalize disposable scenario workspace")?;
            let mut config = LocalServiceConfig::new(
                &home,
                vec![workspace.clone()],
                match options.runtime_mode {
                    RuntimeMode::Offline => LocalServiceMode::Offline,
                    RuntimeMode::Live => LocalServiceMode::Live,
                },
                2,
                manifest.campaign_bounds.max_rounds,
                manifest
                    .campaign_bounds
                    .max_duration_seconds
                    .checked_mul(1_000)
                    .context("campaign duration millisecond overflow")?,
                manifest.campaign_bounds.max_run_tokens,
            )
            .with_model(options.model.clone())?;
            let recorder = if options.runtime_mode == RuntimeMode::Live {
                let recorder = InMemoryObservationRecorder::new(
                    manifest.campaign_bounds.max_provider_requests as usize,
                )
                .map_err(|_| anyhow::anyhow!("provider_observer_configuration_failed"))?;
                let session = ProviderObservationSession::new(
                    EvidenceMode::MetadataOnly,
                    OpaqueScopeId::from_stable_input(
                        campaign_scope.unwrap_or("preflight-observation-scope-v1"),
                    ),
                    recorder.observer(),
                );
                config = config.with_provider_observation(session);
                Some(recorder)
            } else {
                None
            };
            let service = Box::new(LocalService::start(config).await?);
            Ok((
                ActiveTransport::Local {
                    service,
                    _layout: layout,
                    workspace: workspace.display().to_string(),
                },
                recorder,
            ))
        }
        TransportConfig::Attach {
            endpoint,
            token_env,
            workspace,
            ..
        } => {
            let endpoint = validate_attach_endpoint(endpoint)?;
            validate_env_name(token_env)?;
            let token = std::env::var(token_env)
                .map_err(|_| anyhow::anyhow!("attach_token_env_unavailable"))?;
            if token.len() < 16 || token.len() > 8 * 1024 {
                bail!("attach_token_env_invalid");
            }
            Ok((
                ActiveTransport::Attach {
                    endpoint,
                    token,
                    workspace: workspace.clone(),
                },
                None,
            ))
        }
    }
}

fn validate_options(options: &CampaignOptions) -> Result<()> {
    if !options.repository_root.is_absolute()
        || !options.manifest_path.is_absolute()
        || !options.output_root.is_absolute()
    {
        bail!("certification_paths_must_be_absolute");
    }
    if options.artifact_budget_bytes == 0 {
        bail!("campaign_bounds_invalid");
    }
    validate_output_location(&options.output_root, &options.repository_root)?;
    if options.runtime_mode == RuntimeMode::Offline && options.model != DEFAULT_LIVE_MODEL {
        // An offline campaign never reports a provider. Rejecting an ambient
        // alternate profile keeps the CLI behavior explicit and predictable.
        bail!("offline_model_override_not_applicable");
    }
    if options.runtime_mode == RuntimeMode::Live {
        if !live_gate_valid(options.runtime_mode) {
            bail!("live_double_opt_in_missing");
        }
        if matches!(&options.transport, TransportConfig::Local) && !options.live_model_explicit {
            bail!("live_model_must_be_explicit");
        }
        if matches!(&options.transport, TransportConfig::Local) {
            validate_public_model(&options.model)?;
        }
        if ambient_live_override_present_with(|name| std::env::var_os(name).is_some()) {
            bail!("live_ambient_route_or_credential_override_present");
        }
        attest_grok_build_oidc_with_min_validity(
            &options.model,
            ChronoDuration::seconds(LIVE_PREFLIGHT_VALIDITY_SECONDS),
        )
        .map_err(|error| anyhow::anyhow!("live_oidc_attestation_{}", error.code()))?;
    }
    if let TransportConfig::Attach {
        endpoint,
        token_env,
        workspace,
        allow_mutations,
        disposable_target_acknowledged,
    } = &options.transport
    {
        validate_attach_endpoint(endpoint)?;
        validate_env_name(token_env)?;
        if *allow_mutations {
            if workspace.as_deref().is_none_or(str::is_empty) || !disposable_target_acknowledged {
                bail!("attach_mutation_double_opt_in_missing");
            }
            // Current main exposes neither a stable deployment identity nor a
            // service-issued disposable-workspace lease. A caller assertion
            // cannot substitute for service evidence.
            bail!("attach_mutation_lease_unavailable");
        }
    }
    Ok(())
}

fn ambient_live_override_present_with(mut is_present: impl FnMut(&str) -> bool) -> bool {
    LIVE_ROUTE_OVERRIDE_ENVS.iter().any(|name| is_present(name))
}

fn live_gate_valid(mode: RuntimeMode) -> bool {
    mode == RuntimeMode::Offline || std::env::var(LIVE_OPT_IN_ENV).as_deref() == Ok("1")
}

pub fn validate_attach_endpoint(value: &str) -> Result<String> {
    if value.len() > 2_048 || value.contains('\\') {
        bail!("attach_endpoint_invalid");
    }
    let mut url =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("attach_endpoint_invalid"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        bail!("attach_endpoint_invalid");
    }
    if url.scheme() == "http" && !has_literal_loopback_authority(value) {
        bail!("attach_endpoint_requires_https");
    }
    match url.path() {
        "" | "/" => url.set_path(""),
        _ => bail!("attach_endpoint_path_invalid"),
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn has_literal_loopback_authority(value: &str) -> bool {
    let Some((_, remainder)) = value.split_once("://") else {
        return false;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        return host == "::1"
            && (suffix.is_empty()
                || suffix.strip_prefix(':').is_some_and(|port| {
                    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                }));
    }
    let (host, port_valid) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
        ),
        None => (authority, true),
    };
    host == "127.0.0.1" && port_valid
}

fn endpoint_fingerprint(value: &str) -> Result<String> {
    let normalized = validate_attach_endpoint(value)?;
    Ok(format!("opaque-{}", digest(normalized.as_bytes())))
}

fn repository_commit(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(root)
        .output()
        .context("read repository commit")?;
    if !output.status.success() || output.stdout.len() > 80 {
        bail!("repository_commit_unavailable");
    }
    let commit = std::str::from_utf8(&output.stdout)
        .map_err(|_| anyhow::anyhow!("repository_commit_invalid"))?
        .trim()
        .to_owned();
    if !(7..=64).contains(&commit.len())
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("repository_commit_invalid");
    }
    Ok(commit)
}

fn repository_dirty(root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .current_dir(root)
        .output()
        .context("read repository state")?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        bail!("repository_state_unavailable");
    }
    Ok(!output.stdout.is_empty())
}

fn validate_env_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
    {
        bail!("attach_token_env_name_invalid");
    }
    Ok(())
}

fn validate_service_identity(initialized: &serde_json::Value) -> Result<()> {
    if initialized["serverInfo"]["name"].as_str() != Some("grokptah-control") {
        bail!("wrong_service_fingerprint");
    }
    Ok(())
}

pub(crate) fn validate_output_location(output: &Path, repository: &Path) -> Result<()> {
    let repository = dunce::canonicalize(repository).context("canonicalize repository root")?;
    if output.starts_with(&repository)
        && !output.starts_with(repository.join(DEFAULT_OUTPUT_RELATIVE_PATH))
    {
        bail!("output_path_not_in_precise_ignored_root");
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_endpoints_reject_private_url_components() {
        assert_eq!(
            validate_attach_endpoint("https://service.example/").unwrap(),
            "https://service.example"
        );
        assert!(validate_attach_endpoint("http://127.0.0.1:39200").is_ok());
        assert!(validate_attach_endpoint("http://[::1]:39200").is_ok());
        for value in [
            "https://user@service.example",
            "https://service.example?token=secret",
            "https://service.example/#fragment",
            "https://service.example/private",
            "https://service.example/mcp",
            "file:///tmp/socket",
            "http://localhost:39200",
            "http://10.0.0.1:39200",
            "http://2130706433:39200",
            "http://0x7f000001:39200",
            "http://127.1:39200",
            "http://127.0.0.1\\@service.example",
        ] {
            assert!(validate_attach_endpoint(value).is_err());
        }
    }

    #[test]
    fn initialization_requires_the_exact_public_service_fingerprint() {
        assert!(validate_service_identity(&serde_json::json!({
            "serverInfo": {"name": "grokptah-control"}
        }))
        .is_ok());
        assert!(validate_service_identity(&serde_json::json!({
            "serverInfo": {"name": "different-service"}
        }))
        .is_err());
        assert!(validate_service_identity(&serde_json::json!({})).is_err());
    }

    #[test]
    fn smoke_selection_has_only_concrete_implementations() {
        let manifest = CampaignManifest::bundled().unwrap();
        for id in DEFAULT_SMOKE_PROBES {
            assert!(manifest.probe(id).is_some());
            assert!(has_implementation(id));
        }
    }

    #[test]
    fn live_gate_requires_exact_environment_value() {
        let prior = std::env::var_os(LIVE_OPT_IN_ENV);
        unsafe { std::env::set_var(LIVE_OPT_IN_ENV, "true") };
        assert!(!live_gate_valid(RuntimeMode::Live));
        unsafe { std::env::set_var(LIVE_OPT_IN_ENV, "1") };
        assert!(live_gate_valid(RuntimeMode::Live));
        unsafe {
            match prior {
                Some(value) => std::env::set_var(LIVE_OPT_IN_ENV, value),
                None => std::env::remove_var(LIVE_OPT_IN_ENV),
            }
        }
    }

    #[test]
    fn ambient_api_key_base_and_token_command_are_live_safety_refusals() {
        for present in LIVE_ROUTE_OVERRIDE_ENVS {
            assert!(ambient_live_override_present_with(|name| name == *present));
        }
        assert!(!ambient_live_override_present_with(|_| false));
    }

    #[test]
    fn attached_mutations_require_service_issued_disposable_lease() {
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let mut options = default_options(&repository);
        options.transport = TransportConfig::Attach {
            endpoint: "https://service.example".into(),
            token_env: ATTACH_TOKEN_ENV.into(),
            workspace: Some("opaque-workspace".into()),
            allow_mutations: true,
            disposable_target_acknowledged: true,
        };

        let error = validate_options(&options).unwrap_err();
        assert_eq!(error.to_string(), "attach_mutation_lease_unavailable");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offline_preflight_discovers_the_real_public_capacity_surface() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut options = default_options(&repository);
        options.output_root = dunce::canonicalize(output.path()).unwrap();
        let summary = preflight(&options).await.unwrap();
        assert!(summary.manifest_valid);
        assert!(summary.service_reachable);
        assert!(summary.discovered_tool_count > 0);
        assert_eq!(summary.unsupported_probe_count, 0);
        assert_eq!(summary.indeterminate_probe_count, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preflight_never_calls_an_unimplemented_probe_supported() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut options = default_options(&repository);
        options.output_root = dunce::canonicalize(output.path()).unwrap();
        options.selected_probe_ids = vec!["core-checkpoint-inspection-v1".into()];
        let summary = preflight(&options).await.unwrap();
        assert_eq!(summary.unsupported_probe_count, 0);
        assert_eq!(summary.indeterminate_probe_count, 1);
    }

    #[test]
    fn implementation_gap_never_promotes_a_supported_probe() {
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let mut options = default_options(&repository);
        options.runtime_mode = RuntimeMode::Live;
        options.transport = TransportConfig::Attach {
            endpoint: "https://service.invalid".into(),
            token_env: ATTACH_TOKEN_ENV.into(),
            workspace: Some("opaque-test-workspace".into()),
            allow_mutations: true,
            disposable_target_acknowledged: true,
        };
        let transport = ActiveTransport::Attach {
            endpoint: "https://service.invalid".into(),
            token: "opaque-test-token".into(),
            workspace: Some("opaque-test-workspace".into()),
        };
        let manifest = CampaignManifest::bundled().unwrap();
        let probe = manifest.probe("core-checkpoint-inspection-v1").unwrap();
        assert!(!has_implementation(&probe.id));
        let tools: HashSet<_> = grokptah_agent_bridge::discovered_tool_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let capabilities: HashSet<_> = probe.required_capabilities.iter().copied().collect();

        let result =
            classify_probe_availability(probe, &options, &transport, &tools, &capabilities)
                .expect("an implementation gap must be explicit");
        assert_eq!(result.status, ProbeStatus::Indeterminate);
        assert!(result.supported);
        assert_eq!(
            result.diagnostics,
            vec![DiagnosticCode::ProbeImplementationUnavailable]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn merged_native_capability_runs_implemented_policy_probe() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let repository_was_dirty = repository_dirty(&repository).unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut options = default_options(&repository);
        options.output_root = dunce::canonicalize(output.path()).unwrap();
        options.selected_probe_ids = vec!["native-policy-default-off-v1".into()];

        let manifest = CampaignManifest::bundled().unwrap();
        let (transport, recorder) = start_transport(&options, &manifest, None).await.unwrap();
        let mut client = transport.client();
        client.initialize().await.unwrap();
        let tools: HashSet<_> = client
            .list_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        for tool in [
            crate::manifest::FUTURE_SET_MANAGED_EXECUTION_TOOL,
            crate::manifest::FUTURE_GET_MANAGED_EXECUTION_TOOL,
            crate::manifest::FUTURE_AUTHORIZE_WORK_EXECUTION_TOOL,
            crate::manifest::FUTURE_RESOLVE_WORK_INPUT_TOOL,
            crate::manifest::FUTURE_LIST_EXECUTION_INTENTS_TOOL,
        ] {
            assert!(tools.contains(tool), "missing public MCP tool {tool}");
        }
        let capacity = client
            .call_tool("ptah_get_capacity", serde_json::json!({}))
            .await
            .unwrap();
        assert!(!capacity.is_error);
        let health = capacity.structured["health"].as_object().unwrap();
        assert!(health["nativeExecutorError"].is_null());
        let executor = health["nativeExecutor"].as_object().unwrap();
        assert_eq!(executor["enabled"].as_bool(), Some(true));
        assert!(executor["intervalMs"]
            .as_u64()
            .is_some_and(|value| value > 0));
        assert!(executor["startedAt"].as_str().is_some());
        for counter in [
            "admitted",
            "finalized",
            "skippedManual",
            "skippedIneligible",
        ] {
            assert!(
                executor[counter].as_u64().is_some(),
                "missing counter {counter}"
            );
        }
        client.close_session().await.unwrap();
        drop(recorder);
        transport.stop().await;

        let completion = run_campaign(&options).await.unwrap();
        assert_eq!(completion.summary.probes, 1);
        assert_eq!(completion.summary.supported, 1);
        assert_eq!(completion.summary.passed, 1);
        assert_eq!(completion.summary.failed, 0);
        assert_eq!(completion.summary.skipped, 0);
        assert_eq!(completion.summary.indeterminate, 0);
        assert_eq!(completion.certified, !repository_was_dirty);

        let campaign = output.path().join(&completion.campaign_id);
        let report: CampaignReport =
            serde_json::from_slice(&std::fs::read(campaign.join("report.json")).unwrap()).unwrap();
        let probe = report
            .probes
            .iter()
            .find(|probe| probe.probe_id == "native-policy-default-off-v1")
            .unwrap();
        assert_eq!(probe.status, ProbeStatus::Passed);
        assert!(probe.supported);
        assert_eq!(report.repository_dirty, repository_was_dirty);
        assert_eq!(report.certified, !repository_was_dirty);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_work_probe_exercises_offline_public_executor_shape() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let manifest = CampaignManifest::bundled().unwrap();
        let options = default_options(&repository);
        let (transport, recorder) = start_transport(&options, &manifest, None).await.unwrap();
        let workspace = transport.workspace().unwrap().to_owned();
        let mut client = transport.client();
        client.initialize().await.unwrap();
        let definition = manifest.probe("native-work-to-run-v1").unwrap();
        let execution =
            execute_minimal_probe(definition, &mut client, &workspace, None, recorder.as_ref())
                .await;
        assert_eq!(
            execution.result.status,
            ProbeStatus::Passed,
            "diagnostics: {:?}",
            execution.result.diagnostics
        );
        assert!(execution
            .result
            .opaque_ids
            .iter()
            .any(|value| { value.kind == crate::report::DurableIdKind::Work }));
        assert!(execution
            .result
            .opaque_ids
            .iter()
            .any(|value| { value.kind == crate::report::DurableIdKind::Run }));
        assert!(execution.provider_run.is_some());
        client.close_session().await.unwrap();
        drop(recorder);
        transport.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_duplicate_probe_exercises_offline_public_executor_shape() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let manifest = CampaignManifest::bundled().unwrap();
        let options = default_options(&repository);
        let (transport, recorder) = start_transport(&options, &manifest, None).await.unwrap();
        let workspace = transport.workspace().unwrap().to_owned();
        let mut client = transport.client();
        client.initialize().await.unwrap();
        let definition = manifest.probe("native-no-duplicate-run-v1").unwrap();
        let execution =
            execute_minimal_probe(definition, &mut client, &workspace, None, recorder.as_ref())
                .await;
        assert_eq!(
            execution.result.status,
            ProbeStatus::Passed,
            "diagnostics: {:?}",
            execution.result.diagnostics
        );
        assert!(execution.provider_run.is_some());
        client.close_session().await.unwrap();
        drop(recorder);
        transport.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn documented_default_output_root_passes_preflight() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let options = default_options(&repository);
        let summary = preflight(&options).await.unwrap();
        assert!(summary.service_reachable);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offline_smoke_campaign_uses_real_mcp_and_passes_every_selected_probe() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut options = default_options(&repository);
        options.output_root = dunce::canonicalize(output.path()).unwrap();
        let completion = run_campaign(&options).await.unwrap();
        let campaign = output.path().join(&completion.campaign_id);
        let report: CampaignReport =
            serde_json::from_slice(&std::fs::read(campaign.join("report.json")).unwrap()).unwrap();
        let outcomes: Vec<_> = report
            .probes
            .iter()
            .map(|probe| (&probe.probe_id, probe.status, &probe.diagnostics))
            .collect();
        assert_eq!(completion.summary.probes, 7);
        assert_eq!(completion.summary.passed, 7, "{outcomes:?}");
        assert_eq!(completion.summary.failed, 0);
        assert_eq!(completion.summary.skipped, 0);
        assert_eq!(completion.summary.indeterminate, 0);
        let work = report
            .probes
            .iter()
            .find(|probe| probe.probe_id == "work-idempotency-conflict-v1")
            .unwrap();
        assert_eq!(work.counters.errors, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offline_restart_campaign_recovers_run_and_event_cursor() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut options = default_options(&repository);
        options.output_root = dunce::canonicalize(output.path()).unwrap();
        options.selected_probe_ids = vec!["core-restart-durable-runs-events-v1".into()];

        let completion = run_campaign(&options).await.unwrap();
        let campaign = output.path().join(&completion.campaign_id);
        let report: CampaignReport =
            serde_json::from_slice(&std::fs::read(campaign.join("report.json")).unwrap()).unwrap();
        let probe = report
            .probes
            .iter()
            .find(|probe| probe.probe_id == "core-restart-durable-runs-events-v1")
            .unwrap();
        assert_eq!(completion.summary.probes, 1);
        assert_eq!(completion.summary.passed, 1, "{probe:?}");
        assert!(probe.restart.attempted);
        assert!(probe.restart.host_owned);
        assert!(probe.restart.durable_read_recovered);
        assert!(probe.restart.event_cursor_recovered);
        assert!(!probe.restart.implicit_execution_observed);
        assert!(probe.reconnect.continuity_proven);
    }
}
